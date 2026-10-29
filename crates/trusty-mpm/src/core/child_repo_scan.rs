//! Is there a git repository beneath this directory? (#7673)
//!
//! Why: seeding a `CLAUDE.md` into a WORKSPACE PARENT injects it into every
//! project beneath it, so the seed guard must know whether a child repository
//! exists. A scan that stops early and answers "none" makes the guard seed
//! exactly the directory it exists to protect (#7673 round 2 review,
//! CRITICAL), so the answer has three outcomes, not two.
//! What: [`scan_for_child_repo`], a bounded breadth-first walk returning
//! [`ChildRepoScan`]. It is `pub` rather than `pub(crate)` because its natural
//! second consumer, `commands::auto_git_init::ensure_git_repo` (#6274), lives
//! in the `tm` bin target, which sees this library only through its public API.
//! [`scan_for_child_repo_exhaustive`] is the stricter rule the fleet preflight
//! uses before a supervisor grant (#10457).
//! Test: `child_repo_scan_tests.rs`.

use std::collections::VecDeque;
use std::fs::FileType;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Bound on the downward scan: directories visited, not depth.
///
/// Why: a depth bound misses a scoped package three levels down; a visit
/// budget reaches any depth while staying cheap on an ordinary tree.
/// Exhausting it is reported as [`ChildRepoScan::Incomplete`], never as
/// absence (#7673).
/// Test: `a_directory_wider_than_the_budget_is_incomplete`.
pub const WORKSPACE_SCAN_BUDGET: usize = 256;

/// Directory names the scan checks shallowly instead of descending.
///
/// Why: vendor, dependency, build and cache trees are the widest directories
/// in a workspace and the least likely to hold a project. Descending them spent
/// the whole budget before a real child repository was dequeued (#7673 round 2
/// review, HIGH). A skip-listed directory can still BE a repository, or hold
/// one as an immediate child (a vendored git submodule), so it is checked
/// shallowly (#7673 round 3 review, CRITICAL).
/// What: a skip-listed directory is not charged to [`WORKSPACE_SCAN_BUDGET`];
/// its own `.git` and each immediate child's `.git` are checked, and nothing
/// below its immediate children is. `.git` is listed because a repository's
/// own object store never holds a sibling project.
/// Test: `a_wide_node_modules_does_not_hide_a_child_repository`,
/// `a_repository_under_a_skip_listed_name_is_found`,
/// `a_repository_that_is_a_skip_listed_directory_is_found`.
pub const SCAN_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "vendor",
    ".git",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".cache",
];

/// Most immediate children of one skip-listed directory the shallow check
/// probes.
///
/// Why: the shallow check is not charged to [`WORKSPACE_SCAN_BUDGET`], so it
/// needs its own bound, and `node_modules` can hold thousands of packages.
/// 1024 probes cost one `lstat` each, a few milliseconds, and cover a typical
/// hoisted `node_modules`, a Go `vendor/` or a `target/`. A wider directory is
/// [`ChildRepoScan::Incomplete`], never [`ChildRepoScan::Clear`] (#7673).
/// What: counts directory and symlink children only; a plain file cannot hold
/// a `.git` and is not probed.
/// Test: `a_skip_listed_directory_wider_than_its_check_cap_is_incomplete`.
pub const SKIP_DIR_CHECK_CAP: usize = 1024;

/// The outcome of [`scan_for_child_repo`].
///
/// Why: only [`ChildRepoScan::Clear`] proves there is no child repository;
/// a caller must treat [`ChildRepoScan::Incomplete`] as "could not rule one
/// out", never as clear (#7673).
/// Test: `a_directory_wider_than_the_budget_is_incomplete`,
/// `an_unreadable_child_directory_is_incomplete`,
/// `clear_is_constructed_in_one_place`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildRepoScan {
    /// A directory beneath the root contains a `.git` entry.
    Found(PathBuf),
    /// Shallow rule: every directory beneath the root outside a skip-listed
    /// tree was checked, as was every skip-listed directory and its immediate
    /// children; none is a repository. Symlinks are checked but never
    /// descended. Exhaustive rule ([`scan_for_child_repo_exhaustive`]): every
    /// physical directory beneath the root, outside `.git`, was checked, no
    /// symlink leaves the walked tree, and no mount point was met.
    Clear,
    /// The scan stopped before checking everything.
    Incomplete(ScanIncomplete),
}

/// Why a [`ChildRepoScan::Incomplete`] scan stopped.
///
/// What: the I/O error is kept as text so the value stays `Clone + Eq` and
/// can ride inside [`crate::core::claude_md_seed::SeedRefusal`].
/// Test: `the_scan_incomplete_refusal_tells_the_operator_how_to_proceed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanIncomplete {
    /// More than `budget` directories needed checking.
    BudgetExhausted {
        /// The visit budget the scan ran with (#10355: callers differ).
        budget: usize,
    },
    /// A skip-listed directory held more than [`SKIP_DIR_CHECK_CAP`]
    /// directories or symlinks to check.
    SkipDirTooWide {
        /// The skip-listed directory.
        path: PathBuf,
    },
    /// Reading or stat-ing `path` failed: permission denied, or it vanished
    /// mid-walk.
    Unreadable {
        /// The directory or entry that could not be read.
        path: PathBuf,
        /// The I/O error, rendered.
        error: String,
    },
    /// Exhaustive rule: checking `next` would have exceeded `budget`.
    // #10457: names where the walk stopped, so the operator sees which tree is too large.
    BudgetExhaustedAt {
        /// The visit budget the scan ran with.
        budget: usize,
        /// The first directory left unchecked.
        next: PathBuf,
    },
    /// Exhaustive rule: a symlink (to a directory, a file or nothing) resolves
    /// outside the walked tree (outside the root, or into a `.git` directory,
    /// which is not walked).
    SymlinkEscapes {
        /// The symlink.
        link: PathBuf,
        /// Its canonical target.
        target: PathBuf,
    },
    /// Exhaustive rule: a directory beneath the root is on another
    /// filesystem (its `st_dev` differs from the root's).
    MountPoint {
        /// The mounted directory.
        path: PathBuf,
    },
}

impl std::fmt::Display for ScanIncomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExhausted { budget } => {
                write!(f, "the scan stopped after checking {budget} directories")
            }
            Self::SkipDirTooWide { path } => write!(
                f,
                "{} holds more than {SKIP_DIR_CHECK_CAP} directories to check",
                path.display()
            ),
            Self::Unreadable { path, error } => {
                write!(f, "{} could not be read: {error}", path.display())
            }
            Self::BudgetExhaustedAt { budget, next } => write!(
                f,
                "the scan stopped after checking {budget} directories, before {}; the tree is \
                 too large to check",
                next.display()
            ),
            Self::SymlinkEscapes { link, target } => write!(
                f,
                "the symlink {} points to {}, outside the directories the scan walks",
                link.display(),
                target.display()
            ),
            Self::MountPoint { path } => write!(
                f,
                "{} is a mount point on another filesystem, which the scan does not enter",
                path.display()
            ),
        }
    }
}

/// Scan `dir`'s subtree breadth-first for a child git repository.
///
/// What: each real directory beneath `dir` is charged to
/// [`WORKSPACE_SCAN_BUDGET`] and checked for a `.git` entry; a repository is
/// returned without descending, anything else is queued. Names in
/// [`SCAN_SKIP_DIRS`] are checked shallowly and uncharged. A symlink is never
/// descended (no cycle, no escape to `/` or `$HOME`), but a symlink whose
/// target holds a `.git` still counts as [`ChildRepoScan::Found`]. Budget
/// exhaustion, an over-wide skip-listed directory and every I/O error return
/// [`ChildRepoScan::Incomplete`]. A `dir` that does not exist at all queues
/// nothing, so it ends as [`ChildRepoScan::Clear`]. `Clear` is built in one
/// place: after the queue is empty.
/// Test: `a_wide_node_modules_does_not_hide_a_child_repository`,
/// `a_root_that_does_not_exist_yet_is_clear`,
/// `a_directory_wider_than_the_budget_is_incomplete`,
/// `an_unreadable_child_directory_is_incomplete`,
/// `a_symlink_cycle_is_not_descended`,
/// `a_symlink_to_an_outside_directory_is_not_traversed`,
/// `a_symlink_to_a_repository_counts_as_found`,
/// `an_unreadable_symlink_target_is_incomplete`,
/// `a_skip_listed_directory_wider_than_its_check_cap_is_incomplete`,
/// `clear_is_constructed_in_one_place`.
pub fn scan_for_child_repo(dir: &Path) -> ChildRepoScan {
    scan(dir, None, WORKSPACE_SCAN_BUDGET, None)
}

/// [`scan_for_child_repo`], with one entry of the tree left out.
///
/// Why: #8878 R1 — `tm fleet init` accepts one private repository at
/// `<dir>/local` and checks it on its own, but every other repository beneath
/// `dir` must still be found, so the scan cannot stop at `local`.
/// What: the entry whose path equals `except` is neither reported, probed,
/// charged to [`WORKSPACE_SCAN_BUDGET`], nor descended. Everything else is as
/// [`scan_for_child_repo`].
/// Test: `an_excepted_entry_is_skipped_and_its_siblings_are_still_found`.
pub fn scan_for_child_repo_except(dir: &Path, except: &Path) -> ChildRepoScan {
    scan(dir, Some(except), WORKSPACE_SCAN_BUDGET, None)
}

/// The fleet preflight's scan: no skip list, every directory charged to
/// `budget`, and no exit from the tree through a symlink or a mount.
///
/// Why: #10457 — `tm fleet init` grants the supervisor profile over `dir`, so
/// a repository the shallow rule never reads (`node_modules/a/b/.git`, or one
/// behind a symlinked directory) must refuse, not pass. The seed guard and
/// `auto_git_init` keep [`scan_for_child_repo`]: their harm is smaller and the
/// wider walk would raise their false refusals.
/// What: each real directory beneath `dir` is charged to `budget` and
/// descended, skip-listed names included; only `.git` keeps the shallow check
/// of [`SCAN_SKIP_DIRS`]. A directory whose `st_dev` differs from the root's
/// is [`ScanIncomplete::MountPoint`] (a bind mount of the same filesystem
/// shares `st_dev` and is not detected). A directory symlink is probed for
/// `.git` as before, then canonicalized: a target inside the canonical root
/// and outside every `.git` is already walked, so it is skipped and cannot
/// cycle; any other target is [`ScanIncomplete::SymlinkEscapes`]. A file or
/// dangling symlink is fenced the same way on its target as the kernel would
/// resolve it (#10457 ruling (b)), so one inside the root passes; a `.git`
/// component matches in any case. Exhaustion
/// is [`ScanIncomplete::BudgetExhaustedAt`]. `except` is left out as in
/// [`scan_for_child_repo_except`]; the caller checks it, but its `st_dev` is
/// compared here, because only this walk knows the root's. Every I/O error,
/// including a canonicalize or `st_dev` read failure, is
/// [`ScanIncomplete::Unreadable`]. Scaffold limit: a future tm scaffold that
/// symlinks `.claude/*` to a path outside `dir` makes every relaunch refuse.
/// Test: `exhaustive_finds_a_repository_deep_in_a_skip_listed_dir`,
/// `exhaustive_reports_an_escaping_symlink`,
/// `exhaustive_skips_a_symlink_inside_the_root_and_cannot_cycle`,
/// `exhaustive_charges_skip_listed_dirs_to_the_budget`,
/// `exhaustive_still_finds_a_symlink_to_a_repository`,
/// `exhaustive_refuses_a_mount_point_and_an_unreadable_device`,
/// `exhaustive_refuses_an_excepted_entry_on_another_device`,
/// `exhaustive_refuses_a_root_that_vanishes_mid_scan`,
/// `exhaustive_refuses_a_dangling_link_outside_the_root`,
/// `exhaustive_refuses_a_file_link_outside_the_root`,
/// `exhaustive_passes_a_dangling_link_inside_the_root`,
/// `exhaustive_passes_a_file_link_inside_the_root`,
/// `exhaustive_resolves_dotdot_after_a_link_like_the_kernel`,
/// `exhaustive_fences_a_git_component_in_any_case`,
/// `exhaustive_refuses_an_excepted_entry_swapped_for_a_symlink`,
/// `a_tree_wider_than_the_fleet_budget_refuses_in_bounded_time`.
pub fn scan_for_child_repo_exhaustive(
    dir: &Path,
    except: Option<&Path>,
    budget: usize,
) -> ChildRepoScan {
    scan(dir, except, budget, Some(device_of))
}

/// [`scan_for_child_repo_exhaustive`] with the `st_dev` reader named by the
/// caller, so a caller's test can fake a mount point (#10457).
/// Test: `a_mount_point_at_local_is_refused`.
pub fn scan_for_child_repo_exhaustive_with(
    dir: &Path,
    except: Option<&Path>,
    budget: usize,
    device_of: DeviceOf,
) -> ChildRepoScan {
    scan(dir, except, budget, Some(device_of))
}

/// Reads a directory's `st_dev`; a parameter so a test can fake a mount.
pub type DeviceOf = fn(&Path) -> io::Result<u64>;

/// The `st_dev` of `path`, following a symlink; the production [`DeviceOf`].
pub fn device_of(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map(|meta| meta.dev())
}

/// What the exhaustive rule compares each entry against.
struct Fence {
    /// The canonical root: a symlink target under it is already walked.
    real_root: PathBuf,
    /// The root's `st_dev`.
    device: u64,
    device_of: DeviceOf,
}

impl Fence {
    /// `Ok(None)` when `root` does not exist yet: there is nothing to walk.
    fn new(root: &Path, device_of: DeviceOf) -> Result<Option<Self>, ScanIncomplete> {
        let real_root = match std::fs::canonicalize(root) {
            Ok(real) => real,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(unreadable(root, e)),
        };
        let device = device_of(&real_root).map_err(|e| unreadable(root, e))?;
        Ok(Some(Self {
            real_root,
            device,
            device_of,
        }))
    }

    /// Refuse a directory on another filesystem; an unreadable `st_dev` refuses too.
    fn check_mount(&self, dir: &Path) -> Result<(), ScanIncomplete> {
        match (self.device_of)(dir) {
            Ok(device) if device == self.device => Ok(()),
            Ok(_) => Err(ScanIncomplete::MountPoint {
                path: dir.to_path_buf(),
            }),
            Err(e) => Err(unreadable(dir, e)),
        }
    }

    /// Refuse a directory symlink whose canonical target the walk does not reach.
    fn check_link(&self, link: &Path) -> Result<(), ScanIncomplete> {
        let target = std::fs::canonicalize(link).map_err(|e| unreadable(link, e))?;
        self.fence(link, target)
    }

    /// Refuse a file or dangling symlink whose target the walk does not reach,
    /// resolving it through the kernel wherever the kernel has an answer.
    ///
    /// Why: #10457 ruling (b) — a link to a file or to nothing outside `dir`
    /// still lets a read or a write through it leave the tree. Folding `..`
    /// by text is wrong after a symlinked component (the kernel takes the
    /// parent of the link's target), so only the missing tail is folded.
    /// What: a target that exists is canonicalized, as a directory link is.
    /// A dangling one is `read_link` joined to the canonical link parent; its
    /// longest existing prefix is canonicalized and the missing remainder is
    /// appended with `.` and `..` folded. A missing component that is itself
    /// a dangling link inside the root is an entry the walk fences on its own.
    /// Every other stat or canonicalize error is [`ScanIncomplete::Unreadable`].
    fn check_target(&self, link: &Path) -> Result<(), ScanIncomplete> {
        match std::fs::metadata(link) {
            Ok(_) => return self.check_link(link),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(unreadable(link, e)),
        }
        let raw = std::fs::read_link(link).map_err(|e| unreadable(link, e))?;
        let parent = link
            .parent()
            .ok_or_else(|| unreadable(link, io::Error::other("a symlink with no parent")))?;
        let parent = std::fs::canonicalize(parent).map_err(|e| unreadable(link, e))?;
        let full = parent.join(&raw);
        let parts: Vec<Component<'_>> = full.components().collect();
        for split in (1..=parts.len()).rev() {
            let prefix: PathBuf = parts[..split].iter().collect();
            match std::fs::canonicalize(&prefix) {
                Ok(mut target) => {
                    for part in &parts[split..] {
                        match part {
                            Component::ParentDir => {
                                target.pop();
                            }
                            Component::CurDir => {}
                            other => target.push(other),
                        }
                    }
                    return self.fence(link, target);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(unreadable(link, e)),
            }
        }
        Err(unreadable(
            link,
            io::Error::other("no part of the target exists"),
        ))
    }

    /// `target` is walked when it is under the canonical root and outside `.git`.
    fn fence(&self, link: &Path, target: PathBuf) -> Result<(), ScanIncomplete> {
        let walked = target
            .strip_prefix(&self.real_root)
            // #10457: case-insensitive, as APFS resolves `.GIT` to `.git`.
            .is_ok_and(|rel| {
                !rel.components()
                    .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
            });
        if walked {
            return Ok(());
        }
        Err(ScanIncomplete::SymlinkEscapes {
            link: link.to_path_buf(),
            target,
        })
    }
}

fn scan(
    dir: &Path,
    except: Option<&Path>,
    budget: usize,
    exhaustive: Option<DeviceOf>,
) -> ChildRepoScan {
    let mut queue = VecDeque::from([dir.to_path_buf()]);
    let fence = match exhaustive.map(|device_of| Fence::new(dir, device_of)) {
        None => None,
        Some(Ok(Some(fence))) => Some(fence),
        // #10457: a root that does not exist yet holds nothing to walk.
        Some(Ok(None)) => {
            queue.clear();
            None
        }
        Some(Err(stop)) => return ChildRepoScan::Incomplete(stop),
    };
    let mut walk = Walk {
        root: dir,
        except,
        budget,
        visited: 0,
        fence: fence.as_ref(),
    };
    while let Some(current) = queue.pop_front() {
        match walk.level(&current, &mut queue) {
            Ok(None) => {}
            Ok(Some(found)) => return ChildRepoScan::Found(found),
            Err(stop) => return ChildRepoScan::Incomplete(stop),
        }
    }
    // #7673: the only construction of `Clear`; every queued directory was checked.
    ChildRepoScan::Clear
}

/// The state one scan carries from level to level.
struct Walk<'a> {
    root: &'a Path,
    except: Option<&'a Path>,
    budget: usize,
    visited: usize,
    /// `Some` under the exhaustive rule (#10457).
    fence: Option<&'a Fence>,
}

impl Walk<'_> {
    /// Charge one directory to the budget; exhaustion is never `Clear`.
    fn charge(&mut self, next: &Path) -> Result<(), ScanIncomplete> {
        // #10355: the caller names the budget.
        self.visited += 1;
        if self.visited <= self.budget {
            return Ok(());
        }
        Err(match self.fence {
            None => ScanIncomplete::BudgetExhausted {
                budget: self.budget,
            },
            Some(_) => ScanIncomplete::BudgetExhaustedAt {
                budget: self.budget,
                next: next.to_path_buf(),
            },
        })
    }

    /// Whether `name` is checked shallowly: [`SCAN_SKIP_DIRS`] under the
    /// shallow rule, `.git` alone under the exhaustive one (#10457).
    fn skip_listed(&self, name: &std::ffi::OsStr) -> bool {
        match self.fence {
            None => SCAN_SKIP_DIRS.iter().any(|s| name == *s),
            Some(_) => name == ".git",
        }
    }

    /// Check every entry of `current`, queueing the real directories to descend.
    ///
    /// What: `Ok(None)` means every entry was checked, not that the scan is
    /// clear; `Ok(Some)` is a repository; `Err` is "could not finish".
    fn level(
        &mut self,
        current: &Path,
        queue: &mut VecDeque<PathBuf>,
    ) -> Result<Option<PathBuf>, ScanIncomplete> {
        let children = match std::fs::read_dir(current) {
            Ok(children) => children,
            // #7673: a root that does not exist yet (a first-touch seed site the
            // pipeline creates) queues nothing. A CHILD that vanished mid-walk is
            // still incomplete.
            // #10457: under the exhaustive rule the root existed when the fence
            // was built, so a root gone now is unreadable, not empty.
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    && current == self.root
                    && self.fence.is_none() =>
            {
                return Ok(None);
            }
            Err(e) => return Err(unreadable(current, e)),
        };
        for entry in children {
            let entry = entry.map_err(|e| unreadable(current, e))?;
            let path = entry.path();
            // #8878 R1: the caller checks the excepted entry itself.
            if self.except == Some(path.as_path()) {
                // #10457: its device is compared here; its own scan cannot.
                if let Some(fence) = self.fence {
                    let kind = entry.file_type().map_err(|e| unreadable(&path, e))?;
                    // #10457: the caller saw a real directory; one swapped
                    // since (a symlink, a file) is not what it checked.
                    if !kind.is_dir() {
                        let swapped = io::Error::other("no longer a real directory");
                        return Err(unreadable(&path, swapped));
                    }
                    fence.check_mount(&path)?;
                }
                continue;
            }
            // `DirEntry::file_type` does not follow symlinks.
            let kind = entry.file_type().map_err(|e| unreadable(&path, e))?;
            let skip_listed = kind.is_dir() && self.skip_listed(&entry.file_name());
            if kind.is_dir() && !skip_listed {
                self.charge(&path)?;
                // #10457: never cross into another filesystem.
                if let Some(fence) = self.fence {
                    fence.check_mount(&path)?;
                }
            }
            match probe(&path, kind)? {
                Probe::Repository => return Ok(Some(path)),
                // #10457 ruling (b): a file or dangling link is fenced too.
                Probe::NotDirectory if kind.is_symlink() => {
                    if let Some(fence) = self.fence {
                        fence.check_target(&path)?;
                    }
                }
                Probe::NotDirectory => {}
                // #7673: never descend a symlink; `probe` already checked its `.git`.
                // #10457: the exhaustive rule refuses one the walk does not cover.
                Probe::Directory if kind.is_symlink() => {
                    if let Some(fence) = self.fence {
                        fence.check_link(&path)?;
                    }
                }
                // #7673: a skip-listed name is still checked, just not descended.
                Probe::Directory if skip_listed => {
                    if let Some(found) = check_skip_listed(&path)? {
                        return Ok(Some(found));
                    }
                }
                Probe::Directory => queue.push_back(path),
            }
        }
        Ok(None)
    }
}

/// Check a skip-listed directory's immediate children without descending them.
///
/// What: the caller already probed `dir` itself. Each directory or symlink
/// child is probed; plain files are not. More than [`SKIP_DIR_CHECK_CAP`] such
/// children is [`ScanIncomplete::SkipDirTooWide`].
fn check_skip_listed(dir: &Path) -> Result<Option<PathBuf>, ScanIncomplete> {
    let children = std::fs::read_dir(dir).map_err(|e| unreadable(dir, e))?;
    let mut probed = 0usize;
    for entry in children {
        let entry = entry.map_err(|e| unreadable(dir, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| unreadable(&path, e))?;
        if !kind.is_dir() && !kind.is_symlink() {
            continue;
        }
        probed += 1;
        if probed > SKIP_DIR_CHECK_CAP {
            return Err(ScanIncomplete::SkipDirTooWide {
                path: dir.to_path_buf(),
            });
        }
        if let Probe::Repository = probe(&path, kind)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// What one directory entry is, as far as a repository check cares.
enum Probe {
    /// A directory (or a symlink to one) holding a `.git` entry.
    Repository,
    /// A directory (or a symlink to one) with no `.git` entry.
    Directory,
    /// Not a directory: a file, or a symlink to a file or to nothing.
    NotDirectory,
}

/// Classify one entry without descending it; every stat error that is not
/// `NotFound` stops the scan.
///
/// What: a symlink is resolved with `metadata` first; a target that is not a
/// directory, or does not exist, cannot hold a `.git`. A directory, or a
/// symlink to one, is then checked with `symlink_metadata` on its `.git`:
/// `Ok` is a repository, `NotFound` is a plain directory.
fn probe(path: &Path, kind: FileType) -> Result<Probe, ScanIncomplete> {
    if kind.is_symlink() {
        match std::fs::metadata(path) {
            Ok(target) if target.is_dir() => {}
            Ok(_) => return Ok(Probe::NotDirectory),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Probe::NotDirectory),
            Err(e) => return Err(unreadable(path, e)),
        }
    } else if !kind.is_dir() {
        return Ok(Probe::NotDirectory);
    }
    // #7673: `.exists()` folds a stat error into "absent"; map all three ways.
    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(_) => Ok(Probe::Repository),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Probe::Directory),
        Err(e) => Err(unreadable(path, e)),
    }
}

/// An [`ScanIncomplete::Unreadable`] for `path`.
fn unreadable(path: &Path, err: io::Error) -> ScanIncomplete {
    ScanIncomplete::Unreadable {
        path: path.to_path_buf(),
        error: err.to_string(),
    }
}

#[cfg(test)]
#[path = "child_repo_scan_tests.rs"]
mod tests;
