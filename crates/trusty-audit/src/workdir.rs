//! The working directory `trusty-audit` owns.
//!
//! Why: the crate downloads pinned tools, clones the recipient's repositories,
//! builds a tga extract database, and records run progress. Every one of those
//! is a file it writes on someone else's machine, and #5502 designs the layout
//! now rather than retrofitting one once four features have each picked their
//! own path. Two properties follow from designing it here:
//!
//! - **Everything is under one root.** `rm -rf <root>` is a complete uninstall
//!   because no path this crate writes escapes the root — that is the property
//!   `layout_tests::every_layout_path_is_inside_the_root` proves, and it is what
//!   the README's deletion instructions rest on.
//! - **The root holds the recipient's source.** `repos/` is their checkouts and
//!   `extract/` is derived from them, so this is a data-handling surface. The
//!   README states what is written where (#5494).
//!
//! What: [`WorkDir`], a pure path calculator — it computes the layout without
//! touching disk, and [`WorkDir::create`] is the one method on it that does I/O.
//! [`WorkDir::resolve`] takes the environment as an argument rather than reading
//! it, so resolution order is testable without mutating process state.
//!
//! [`write_atomically`] is the module's one free function, and the one writer
//! for every state file under the root: `state/selected-repos.toml`
//! (`crate::run`) and `state/audit-targets.toml` (`crate::registry`) both go
//! through it, so the temp-file-then-rename discipline is decided once (#5822).
//! [`write_private_atomically`] is the same writer at mode 0600, for the one
//! file that carries a credential (#5868).
//! Test: `super::layout_tests`.
//!
//! ## Open questions, recorded rather than resolved (#5502)
//!
//! 1. **Where the root should live.** The scaffold's default is
//!    `<cwd>/trusty-audit-work`, chosen because the handoff arrives as an
//!    unzipped directory the recipient already has open (#5473) — so the audit's
//!    files sit beside the thing they unzipped instead of in a hidden home
//!    directory they were never told about. The alternative,
//!    `~/.trusty-tools/trusty-audit/`, matches the rest of this workspace's
//!    convention and survives the recipient deleting the unzipped folder. Not
//!    settled; the default here is a placeholder, and `--work-dir` plus
//!    [`WORKDIR_ENV`] make it overridable meanwhile.
//! 2. **Whether two runs may share one root.** Nothing here locks, and the
//!    scaffold does not decide. Sharing is attractive (a second engagement
//!    reuses the downloaded tools) and dangerous (`state/` and `out/` are
//!    per-run). A lock file or a per-engagement subdirectory both answer it.
//! 3. **Deletion semantics.** Deleting the root mid-run loses run progress and
//!    the clones; nothing outside the root is affected. Whether a `clean` verb
//!    should exist — and whether it should refuse to run while a run is in
//!    flight — is open.

use std::path::{Path, PathBuf};

use crate::error::AuditError;

/// Environment variable that overrides the default working-directory root.
pub const WORKDIR_ENV: &str = "TRUSTY_AUDIT_WORKDIR";

/// Directory name appended to the current directory when nothing overrides it.
pub const DEFAULT_WORKDIR_NAME: &str = "trusty-audit-work";

/// One entry in the working-directory layout.
///
/// Why: the CLI prints the layout, the README documents it, and the containment
/// test walks it — all three want the same list, so the list is data.
/// What: a stable identifier plus one line of what lands there.
/// Test: `super::layout_tests::every_layout_path_is_inside_the_root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Area {
    /// Pinned tool binaries, installed via `trusty-installer` (#5491).
    Tools,
    /// Clones of the recipient's repositories — their source code.
    Repos,
    /// The tga extract database, derived from those clones.
    Extract,
    /// Run state: repo selection and per-repo progress (#5494).
    State,
    /// The deliverable that goes back: report plus manifest. Never a credential.
    Output,
    /// Logs from the tools this crate runs.
    Logs,
}

impl Area {
    /// Every area, in the order the CLI and README present them.
    pub const ALL: [Area; 6] = [
        Area::Tools,
        Area::Repos,
        Area::Extract,
        Area::State,
        Area::Output,
        Area::Logs,
    ];

    /// Directory name under the root.
    pub fn dir_name(self) -> &'static str {
        match self {
            Area::Tools => "tools",
            Area::Repos => "repos",
            Area::Extract => "extract",
            Area::State => "state",
            Area::Output => "out",
            Area::Logs => "logs",
        }
    }

    /// One line describing what is written there, for the CLI and the README.
    pub fn description(self) -> &'static str {
        match self {
            Area::Tools => "pinned tga / trusty-analyze / trusty-review binaries",
            Area::Repos => "clones of your repositories — your source code",
            Area::Extract => "the tga extract database built from those clones",
            Area::State => "repo selection and run progress",
            Area::Output => "the deliverable to return — report and manifest, never a key",
            Area::Logs => "output from the tools this client runs",
        }
    }
}

/// The directory tree `trusty-audit` owns on the recipient's machine.
///
/// Why: see the module docs — one root so deletion is complete, and a pure path
/// calculator so the layout can be asserted without a filesystem.
/// What: wraps an absolute-or-relative root; every accessor is `root.join(...)`.
/// Test: `super::layout_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkDir {
    root: PathBuf,
}

impl WorkDir {
    /// Wrap an explicit root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Decide the root from the flag, then the environment, then the default,
    /// and anchor it to `cwd` when it is relative.
    ///
    /// Why: the caller (`main.rs`) reads the process environment once and passes
    /// it in, which keeps this function pure — resolution order is provable
    /// without `std::env::set_var` and its cross-test races.
    ///
    /// Anchoring is what makes the root usable by a CHILD process (#5672).
    /// `run::sweep` spawns `tga audit` with the root as the child's working
    /// directory, and a relative program path is resolved against the CHILD's
    /// cwd, not the parent's — so `--work-dir w` named `w/tools/tga` to a child
    /// already sitting in `w`, and the spawn failed with `os error 2`. Every
    /// path this crate hands a child descends from the root, so anchoring the
    /// root once here fixes the tool binaries, the generated tga config, the
    /// output directory and the extract database together.
    ///
    /// What: `explicit` wins; else `env_value` (the value of [`WORKDIR_ENV`]);
    /// else [`DEFAULT_WORKDIR_NAME`]; then `cwd.join(...)`, which returns an
    /// absolute choice unchanged. An empty env value is ignored, since
    /// `TRUSTY_AUDIT_WORKDIR=` is far more likely to be an unset shell variable
    /// than a request to use the filesystem root.
    ///
    /// This joins rather than calling `std::fs::canonicalize`, deliberately.
    /// The root normally does not exist yet — [`WorkDir::create`] makes it — and
    /// `canonicalize` fails on a path that is not already on disk, so it cannot
    /// run at this boundary at all. It would also resolve symlinks, which would
    /// print a root the recipient never typed and weaken the README's
    /// `rm -rf <root>` instruction. Joining needs no I/O and cannot fail.
    /// Test: `super::layout_tests::resolution_order_is_flag_then_env_then_default`,
    /// `super::layout_tests::a_relative_choice_is_anchored_to_the_cwd`.
    pub fn resolve(explicit: Option<PathBuf>, env_value: Option<&str>, cwd: &Path) -> Self {
        let chosen = explicit
            .or_else(|| {
                env_value
                    .filter(|v| !v.trim().is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from(DEFAULT_WORKDIR_NAME));
        Self::new(cwd.join(chosen))
    }

    /// The root of everything this crate writes.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of one layout area.
    ///
    /// This computes a path; it does not check what is at it. A pre-planted
    /// symlink at an area would send writes outside the root and survive the
    /// delete — `tools::install` refuses that before installing (#5495), and
    /// repo cloning owes the same check when it lands (#5215).
    pub fn path(&self, area: Area) -> PathBuf {
        self.root.join(area.dir_name())
    }

    /// Every area paired with its path, in [`Area::ALL`] order.
    pub fn layout(&self) -> Vec<(Area, PathBuf)> {
        Area::ALL.iter().map(|a| (*a, self.path(*a))).collect()
    }

    /// Create the root and every area directory, idempotently.
    ///
    /// Why: the guided flow's first act is making the working directory exist,
    /// and a partially-created tree is worse than none — a later feature that
    /// assumes `state/` exists would fail somewhere unrelated to the cause.
    /// What: `create_dir_all` per area, which also creates the root. Existing
    /// directories are left alone, so re-running a flow is safe.
    /// Test: `super::layout_tests::create_is_idempotent_and_makes_every_area`.
    ///
    /// # Errors
    ///
    /// [`AuditError::WorkDir`] naming the first path that could not be created.
    pub fn create(&self) -> Result<(), AuditError> {
        for (_, path) in self.layout() {
            std::fs::create_dir_all(&path).map_err(|source| AuditError::WorkDir {
                path: path.clone(),
                source,
            })?;
        }
        Ok(())
    }
}

/// Write a state file so no reader can ever observe a partial one.
///
/// Why: `crate::run::SELECTION_FILE` states this obligation on whoever writes
/// it — a producer that crashes mid-write leaves syntactically valid TOML
/// holding a PREFIX of the entries, which reads as a smaller-but-complete
/// document. #5822 adds a second such file (`crate::registry`), so the
/// discipline moved here rather than being restated per producer. #5494 adds a
/// third, and the one that most needs it: the run checkpoint is written after
/// every repository precisely so it survives the crash it exists for, and a
/// torn one would tell the next run a four-hour audit had finished.
/// What: creates the parent directory, writes to a uniquely-named temporary
/// file beside the target, and renames it into place. The rename is atomic; the
/// unique suffix is what lets two writers race without either reading the
/// other's half-written file. A failed rename removes the temporary file.
/// Test: `crate::run::run_tests::racing_writers_never_leave_a_torn_selection`,
/// `crate::registry::registry_tests::a_registry_round_trips_both_kinds`,
/// `super::layout_tests::an_atomic_write_leaves_no_temporary_behind`,
/// `super::layout_tests::an_unpublishable_target_is_an_error_and_leaves_no_temporary`.
///
/// This makes ONE write untearable. It does not make a load-mutate-save
/// indivisible — [`crate::registry::register`] takes a lock for that (#5822).
///
/// # Errors
///
/// [`AuditError::WorkDir`] naming the directory, the temporary file, or the
/// target, depending on which step failed.
pub(crate) fn write_atomically(path: &Path, text: &str) -> Result<(), AuditError> {
    write_with_mode(path, text, None)
}

/// Write a file only its owner can read, without ever tearing it.
///
/// Why: `engagement.toml` carries the OpenRouter credential, and the first-run
/// prompt persists it there (#5868). Under a default 022 umask the plain writer
/// above leaves that file 0644 — readable by every account on the machine,
/// which on a client's shared build host is the whole exposure. The recipient
/// did not choose the mode and should not have to know to check it.
/// What: [`write_atomically`] with the temporary file created at 0600. The mode
/// is applied to the TEMPORARY file, before the rename, so the credential is
/// never observable at the target path through a wider mode — not even for the
/// instant between two syscalls.
/// Test: `super::layout_tests::a_private_write_is_owner_only_and_still_atomic`.
///
/// # Errors
///
/// As [`write_atomically`].
pub(crate) fn write_private_atomically(path: &Path, text: &str) -> Result<(), AuditError> {
    write_with_mode(path, text, Some(OWNER_ONLY))
}

/// Mode a file holding a credential is created with: owner read/write, nothing else.
#[cfg(unix)]
const OWNER_ONLY: u32 = 0o600;
/// Windows has no mode bits; the constant exists so the signature is uniform.
#[cfg(not(unix))]
const OWNER_ONLY: u32 = 0;

/// The shared temp-file-then-rename writer. See [`write_atomically`].
fn write_with_mode(path: &Path, text: &str, mode: Option<u32>) -> Result<(), AuditError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|source| AuditError::WorkDir {
        path: dir.to_path_buf(),
        source,
    })?;

    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = path.with_file_name(format!("{file_name}.{}.tmp", writer_tag()));
    write_temp(&temp, text, mode).map_err(|source| AuditError::WorkDir {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, path).map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        AuditError::WorkDir {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// Create the temporary file holding `text`, at `mode` when one is asked for.
///
/// `OpenOptions::mode` decides the permissions the file is CREATED with, so
/// there is no window in which it exists at the umask's default. The explicit
/// `set_permissions` afterwards covers the one case `mode` does not: a
/// leftover temporary from a crashed writer whose tag this one reused, which
/// `create` opens rather than creates and therefore leaves at its old mode.
#[cfg(unix)]
fn write_temp(temp: &Path, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    if let Some(mode) = mode {
        options.mode(mode);
    }
    let mut file = options.open(temp)?;
    if let Some(mode) = mode {
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    file.write_all(text.as_bytes())
}

/// See the unix arm. Windows carries no mode bits, so the request is dropped.
#[cfg(not(unix))]
fn write_temp(temp: &Path, text: &str, _mode: Option<u32>) -> std::io::Result<()> {
    std::fs::write(temp, text)
}

/// A suffix no two concurrent writers share: process, plus thread within it.
fn writer_tag() -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    format!("{}-{}", std::process::id(), hasher.finish())
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn resolution_order_is_flag_then_env_then_default() {
        let cwd = Path::new("/engagement");

        let flag = WorkDir::resolve(Some(PathBuf::from("/flag")), Some("/env"), cwd);
        assert_eq!(flag.root(), Path::new("/flag"));

        let env = WorkDir::resolve(None, Some("/env"), cwd);
        assert_eq!(env.root(), Path::new("/env"));

        let default = WorkDir::resolve(None, None, cwd);
        assert_eq!(default.root(), Path::new("/engagement/trusty-audit-work"));
    }

    /// #5672: a relative choice from either source becomes absolute, because
    /// `run::sweep` hands these paths to a child running in a different cwd.
    #[test]
    fn a_relative_choice_is_anchored_to_the_cwd() {
        let cwd = Path::new("/engagement");

        let flag = WorkDir::resolve(Some(PathBuf::from("w")), None, cwd);
        assert_eq!(flag.root(), Path::new("/engagement/w"));

        let env = WorkDir::resolve(None, Some("w"), cwd);
        assert_eq!(env.root(), Path::new("/engagement/w"));
    }

    #[test]
    fn a_blank_env_value_falls_through_to_the_default() {
        let cwd = Path::new("/engagement");
        let resolved = WorkDir::resolve(None, Some("   "), cwd);
        assert_eq!(resolved.root(), Path::new("/engagement/trusty-audit-work"));
    }

    /// The property the README's "delete the directory" instruction rests on.
    #[test]
    fn every_layout_path_is_inside_the_root() {
        let work = WorkDir::new("/engagement/trusty-audit-work");
        for (area, path) in work.layout() {
            assert!(
                path.starts_with(work.root()),
                "{area:?} escapes the root: {}",
                path.display()
            );
        }
    }

    #[test]
    fn area_names_are_distinct() {
        let mut names: Vec<&str> = Area::ALL.iter().map(|a| a.dir_name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two areas share a directory name");
    }

    /// The temporary the rename publishes from must never outlive the write —
    /// a `state/` littered with `.tmp` files is how a reader learns to guess.
    #[test]
    fn an_atomic_write_leaves_no_temporary_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("state/run-progress.toml");

        write_atomically(&target, "complete = false\n").expect("first write");
        write_atomically(&target, "complete = true\n").expect("overwrite");

        assert_eq!(
            std::fs::read_to_string(&target).expect("reads"),
            "complete = true\n"
        );
        let leftovers: Vec<PathBuf> = std::fs::read_dir(tmp.path().join("state"))
            .expect("read state")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// #5868: the file the credential lands in is owner-only, and it gets there
    /// through the same untearable rename as every other state file. Both
    /// halves matter — a 0600 file written non-atomically is still a file a
    /// reader can catch half-written.
    #[cfg(unix)]
    #[test]
    fn a_private_write_is_owner_only_and_still_atomic() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("engagement.toml");

        // Land on an existing world-readable file: the mode must be replaced,
        // not inherited, because that is the shape a hand-edited config has.
        std::fs::write(&target, "openrouter_key = \"\"\n").expect("seed");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        write_private_atomically(&target, "openrouter_key = \"sk-or-v1-x\"\n").expect("write");

        let mode = std::fs::metadata(&target).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        assert_eq!(
            std::fs::read_to_string(&target).expect("reads"),
            "openrouter_key = \"sk-or-v1-x\"\n"
        );
        let leftovers: Vec<PathBuf> = std::fs::read_dir(tmp.path())
            .expect("read dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A target that cannot be published is an error, never a silent no-op —
    /// the caller has to be able to refuse over it.
    #[test]
    fn an_unpublishable_target_is_an_error_and_leaves_no_temporary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("state/run-progress.toml");
        // A non-empty directory at the target: the rename cannot replace it.
        std::fs::create_dir_all(&target).expect("mkdir");
        std::fs::write(target.join("occupied"), b"x").expect("write");

        let err = write_atomically(&target, "complete = true\n")
            .expect_err("a directory cannot be replaced by a rename");
        assert!(matches!(err, AuditError::WorkDir { .. }), "{err:?}");
        let leftovers: Vec<PathBuf> = std::fs::read_dir(tmp.path().join("state"))
            .expect("read state")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn create_is_idempotent_and_makes_every_area() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));

        work.create().expect("first create");
        work.create().expect("second create is a no-op");

        for (area, path) in work.layout() {
            assert!(
                path.is_dir(),
                "{area:?} was not created: {}",
                path.display()
            );
        }
    }
}
