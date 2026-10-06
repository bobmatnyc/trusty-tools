//! Staging trees beside a repo's slots: the bounded clone, the all-slot sweep,
//! and the read-only listing `tm build-lease status` prints (#9239).
//!
//! Why: a seed clones the repo's whole shared target directory with
//! `cp -c -R`. Under I/O pressure that clone ran 30-76 minutes while the lease
//! holder waited on it with no bound, and every other builder waited on that
//! lease. A killed seed also left its staging tree behind, and the sweep only
//! looked at the index being seeded, so orphans under other indexes piled up
//! (one held 238 GiB).
//! What: [`clone_directory`] waits for the clone until a deadline and then
//! kills it; [`discard`] moves a tree out of the way with one rename and
//! deletes it on a background thread; [`sweep_abandoned_staging`] discards
//! every dead owner's tree for EVERY slot index; [`pool_status`] lists the
//! slots and the trees beside them.
//! Test: `staging_tests.rs`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{
    STAGING_PREFIX_DOT, UNSUPPORTED_PREFIX, hold_idle_build_locks, marker_reads_seeded,
    unique_nanos,
};
use crate::core::build_lease::evict::{EVICTING_INFIX, repo_dirs};

/// The infix a seed's private staging tree carries: `.slot-<n>.seeding.<pid>.<nanos>`.
const SEEDING_INFIX: &str = ".seeding.";

/// How often the bounded clone checks on `cp`.
const POLL: Duration = Duration::from_millis(50);

/// How long a killed `cp` gets to exit before a thread reaps it instead.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// Builds the clone command for `(src, staging)`.
///
/// Why: the default is `cp -c -R`; a test swaps in a command that never
/// finishes, so the bound is proven without a 30-minute clone (#9239).
pub(super) type Cloner = fn(&Path, &Path) -> Command;

/// The production clone: `cp -c -R <src> <staging>`.
pub(super) fn cp_clone(src: &Path, staging: &Path) -> Command {
    let mut cmd = Command::new("cp");
    cmd.arg("-c").arg("-R").arg(src).arg(staging);
    cmd
}

/// This run's private staging directory for `dst`.
///
/// Why: a fixed staging name is a shared mutable directory between any two seed
/// runs, and the first act of a seed is to clear it — so the later run deletes
/// the earlier one's tree mid-copy. The in-daemon registry cannot prevent that
/// pairing across a restart: `std::process::Command` sets no death signal (macOS
/// has none), so a `cp -c -R` outlives the daemon that spawned it while the new
/// daemon's registry starts empty (#8261 critic round 3). PID plus nanoseconds
/// is unique across both processes and repeated seeds within one.
/// What: `<parent>/.<slot>.seeding.<pid>.<nanos>`.
/// Test: `two_staging_trees_for_one_index_publish_exactly_one_slot`.
fn staging_path(parent: &Path, slot: &str) -> PathBuf {
    parent.join(format!(
        "{STAGING_PREFIX_DOT}{slot}{SEEDING_INFIX}{}.{}",
        std::process::id(),
        unique_nanos()
    ))
}

/// Why a clone did not land, sorted by what the seed should do next (#8794).
#[derive(Debug)]
pub(super) enum CloneError {
    /// This platform cannot clone at all: a cold slot is the documented outcome.
    Unsupported(String),
    /// Another run published the slot first: the warm case.
    Published,
    /// A live build holds this lock in the slot: refuse, touch nothing.
    ActiveBuild(PathBuf),
    /// The clone or the replace failed: no marker, retry next time.
    Failed(String),
    /// #9239: the clone missed its deadline and was killed; its staging tree
    /// is discarded and `dst` is untouched.
    TimedOut(Duration),
}

/// Copy-on-write clone `src` to `dst` before `deadline`, or say why not.
///
/// Why: `cp -c` is the only portable way to ask APFS for a clone from Rust
/// without an `fcntl`/`clonefile` binding. It is NOT seconds of wall clock on
/// a large tree: measured at 30-76 minutes under I/O pressure (#9239), which is
/// why it now runs against a deadline.
/// What: runs the clone into a private staging sibling, polling until it exits
/// or `deadline` passes. Past the deadline `cp` is killed, the staging tree is
/// discarded, and the result is [`CloneError::TimedOut`] with `dst` untouched.
/// On success the staging tree is renamed into place under the held cargo
/// locks of `dst`. Every non-macOS platform returns [`CloneError::Unsupported`].
///
/// **Never `cp -c -R <src> <dst>` onto an existing `dst`.** BSD `cp` then writes
/// `dst/<basename(src)>` and still exits 0, so the slot would read as cloned
/// with an empty top level and a nested copy underneath (#8261 critic round).
/// The staging tree has no such case, and the rename is atomic.
///
/// **A live build's directory is never replaced (#8794).** The cargo locks in
/// `dst` are taken right before the replace and held until the clone is in
/// place; a held one aborts with [`CloneError::ActiveBuild`].
///
/// # Errors
///
/// [`CloneError`], which tells the caller whether a cold slot, a refusal, or an
/// unseeded retry is the right outcome.
///
/// Test: `a_clone_past_its_deadline_is_killed_and_discarded`,
/// `two_staging_trees_for_one_index_publish_exactly_one_slot`,
/// `a_slot_with_a_held_cargo_lock_is_not_replaced`,
/// `a_failed_replace_leaves_no_seeded_marker`.
pub(super) fn clone_directory(
    src: &Path,
    dst: &Path,
    cloner: Cloner,
    deadline: Instant,
) -> Result<(), CloneError> {
    if !cfg!(target_os = "macos") {
        return Err(CloneError::Unsupported(format!(
            "{UNSUPPORTED_PREFIX}; this is {}",
            std::env::consts::OS
        )));
    }
    let (Some(parent), Some(slot)) = (
        dst.parent(),
        dst.file_name().and_then(std::ffi::OsStr::to_str),
    ) else {
        return Err(CloneError::Failed(format!(
            "{} has no parent directory or slot name",
            dst.display()
        )));
    };
    if let Err(err) = std::fs::create_dir_all(parent) {
        return Err(CloneError::Failed(format!(
            "could not create {}: {err}",
            parent.display()
        )));
    }
    let staging = staging_path(parent, slot);
    let started = Instant::now();
    // `cp -c` fails outright rather than falling back to a full byte copy when
    // the volume cannot clone: a silent 207 GB real copy would fill the disk.
    let mut child = cloner(src, &staging)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| CloneError::Failed(format!("could not run cp: {err}")))?;
    // #9239: drained on a thread so a chatty `cp` cannot fill the pipe and stall.
    let stderr = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut text = String::new();
            drop(pipe.read_to_string(&mut text));
            text
        })
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // #9239: the bound is the point — kill it, never wait it out.
                abandon(child, &staging);
                return Err(CloneError::TimedOut(started.elapsed()));
            }
            Ok(None) => {
                std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(err) => {
                abandon(child, &staging);
                return Err(CloneError::Failed(format!("could not wait for cp: {err}")));
            }
        }
    };
    let stderr = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    if !status.success() {
        discard_logged(&staging);
        return Err(CloneError::Failed(format!(
            "cp -c exited {status}: {}",
            stderr.trim()
        )));
    }
    // A run that survived a daemon restart can have published this slot while
    // `cp` ran; replacing a MARKED directory would destroy a tree a builder holds.
    if marker_reads_seeded(dst) {
        discard_logged(&staging);
        return Err(CloneError::Published);
    }
    // #8794: a build that started meanwhile is caught here, and the locks stay
    // held until the clone is in place.
    let _locks = match hold_idle_build_locks(dst) {
        Ok(locks) => locks,
        Err(lock) => {
            discard_logged(&staging);
            return Err(CloneError::ActiveBuild(lock));
        }
    };
    // #9239: one rename, not an inline `remove_dir_all` of an unbounded tree.
    if dst.exists()
        && let Err(err) = discard(dst)
    {
        discard_logged(&staging);
        return Err(CloneError::Failed(format!(
            "could not replace the unseeded {}: {err}",
            dst.display()
        )));
    }
    std::fs::rename(&staging, dst).map_err(|err| {
        discard_logged(&staging);
        CloneError::Failed(format!(
            "could not move the clone into {}: {err}",
            dst.display()
        ))
    })
}

/// Kill a clone that missed its deadline and discard its staging tree (#9239).
///
/// What: SIGKILL, then up to [`KILL_GRACE`] for `cp` to exit — a process in
/// uninterruptible I/O dies only when its syscall returns, so past the grace a
/// thread reaps it and this returns anyway. The staging tree is discarded
/// whether or not `cp` has exited yet.
fn abandon(mut child: std::process::Child, staging: &Path) {
    drop(child.kill());
    let grace = Instant::now() + KILL_GRACE;
    while Instant::now() < grace && matches!(child.try_wait(), Ok(None)) {
        std::thread::sleep(POLL);
    }
    if matches!(child.try_wait(), Ok(None)) {
        std::thread::spawn(move || drop(child.wait()));
    }
    discard_logged(staging);
}

/// A tree moved aside by [`discard`], with the thread deleting it.
#[derive(Debug)]
pub struct Discarded {
    /// Where the tree now lives: `.slot-<n>.evicting.<pid>.<nanos>`.
    pub path: PathBuf,
    /// The background deletion; `None` when no thread could be started, in
    /// which case the tree waits for the next sweep.
    pub remover: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

/// Move `tree` out of the way with one rename, then delete it off this thread.
///
/// Why (#9239): `remove_dir_all` of a 238 GiB clone is itself unbounded, so it
/// never runs on a path a builder waits on. The new name carries THIS pid, so
/// no other sweeper touches it while this process lives, and a later sweep
/// reclaims it if this process dies first.
/// What: renames to `<parent>/.slot-<n>.evicting.<pid>.<nanos>` — the name
/// `build_lease::evict` already uses for a tree it is deleting — and spawns a
/// thread running `remove_dir_all`.
///
/// # Errors
///
/// The rename's error; the tree is then left where it was, under its old name.
///
/// Test: `a_dead_owners_staging_under_another_index_is_swept`.
pub fn discard(tree: &Path) -> std::io::Result<Discarded> {
    let parent = tree.parent().ok_or_else(|| {
        std::io::Error::other(format!("{} has no parent directory", tree.display()))
    })?;
    let name = tree.file_name().and_then(std::ffi::OsStr::to_str);
    let slot = name
        .and_then(slot_index_of)
        .map_or_else(|| "x".to_string(), |n| n.to_string());
    let doomed = parent.join(format!(
        ".slot-{slot}{EVICTING_INFIX}{}.{}",
        std::process::id(),
        unique_nanos()
    ));
    std::fs::rename(tree, &doomed)?;
    let target = doomed.clone();
    let remover = std::thread::Builder::new()
        .name("slot-discard".to_string())
        .spawn(move || {
            let removed = std::fs::remove_dir_all(&target);
            if let Err(err) = &removed {
                tracing::warn!(tree = %target.display(), "could not delete a discarded builder-slot tree; the next sweep retries: {err}");
            }
            removed
        })
        .inspect_err(|err| tracing::warn!(tree = %doomed.display(), "no thread to delete a discarded builder-slot tree; the next sweep retries: {err}"))
        .ok();
    Ok(Discarded {
        path: doomed,
        remover,
    })
}

/// [`discard`], logging a failure instead of returning it.
fn discard_logged(tree: &Path) {
    if let Err(err) = discard(tree)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(tree = %tree.display(), "could not discard a builder-slot staging tree: {err}");
    }
}

/// The slot index a `slot-<n>` or `.slot-<n>.<…>` name carries.
fn slot_index_of(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix(".slot-")
        .or_else(|| name.strip_prefix("slot-"))?;
    rest.split('.').next()?.parse().ok()
}

/// What a staging-shaped tree beside the slots is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StagingKind {
    /// `.slot-<n>.seeding.<pid>.<nanos>`: a seed's clone in progress, or
    /// abandoned when its owner is dead.
    Seeding,
    /// `.slot-<n>.evicting.<pid>.<nanos>`: a tree being deleted.
    Discarding,
}

/// One staging or discard tree beside a repo's slots (#9239).
///
/// Test: `a_staging_name_parses_into_slot_pid_and_age`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StagingEntry {
    /// The tree.
    pub path: PathBuf,
    /// The slot index its name carries.
    pub slot: u32,
    /// Seeding or being deleted.
    pub kind: StagingKind,
    /// The owner pid its name carries.
    pub pid: u32,
    /// When its owner started it, from the name's nanoseconds.
    pub started: Option<SystemTime>,
    /// Whether the owner is this process or a live one.
    pub alive: bool,
}

impl StagingEntry {
    /// Parse a staging-shaped `path`; `None` for any other name.
    #[must_use]
    pub fn parse(path: PathBuf) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        let rest = name.strip_prefix(".slot-")?;
        let (index, rest, kind) = if let Some((i, r)) = rest.split_once(SEEDING_INFIX) {
            (i, r, StagingKind::Seeding)
        } else {
            let (i, r) = rest.split_once(EVICTING_INFIX)?;
            (i, r, StagingKind::Discarding)
        };
        let slot = index.parse().ok()?;
        let (pid, nanos) = rest.split_once('.')?;
        let pid = pid.parse::<u32>().ok()?;
        let started = nanos
            .parse::<u64>()
            .ok()
            .map(|n| UNIX_EPOCH + Duration::from_nanos(n));
        let alive = pid == std::process::id() || crate::core::process::is_process_alive(pid);
        Some(Self {
            path,
            slot,
            kind,
            pid,
            started,
            alive,
        })
    }

    /// How long ago the owner started this tree.
    #[must_use]
    pub fn age(&self) -> Option<Duration> {
        self.started
            .and_then(|s| SystemTime::now().duration_since(s).ok())
    }

    /// One status line: what, which slot, which pid, alive or dead, how old.
    #[must_use]
    pub fn render(&self) -> String {
        let what = match self.kind {
            StagingKind::Seeding => "seeding",
            StagingKind::Discarding => "discarding",
        };
        let owner = if self.alive {
            "alive"
        } else {
            "dead; the next lease on this repo sweeps it"
        };
        let age = self.age().map_or_else(
            || "age unknown".to_string(),
            |a| format!("age {}m{:02}s", a.as_secs() / 60, a.as_secs() % 60),
        );
        format!(
            "{what} slot {}: pid {} ({owner}), {age}, {}",
            self.slot,
            self.pid,
            self.path.display()
        )
    }
}

/// Every staging or discard tree directly in `repo_dir`; none when unreadable.
#[must_use]
pub fn staging_entries(repo_dir: &Path) -> Vec<StagingEntry> {
    let Ok(entries) = std::fs::read_dir(repo_dir) else {
        return Vec::new();
    };
    let mut found: Vec<StagingEntry> = entries
        .flatten()
        .filter_map(|entry| StagingEntry::parse(entry.path()))
        .collect();
    found.sort_by(|a, b| a.slot.cmp(&b.slot).then_with(|| a.path.cmp(&b.path)));
    found
}

/// What one sweep did.
#[derive(Debug, Default)]
pub struct SweepReport {
    /// Trees moved aside and being deleted.
    pub discarded: Vec<Discarded>,
    /// Trees that could not be moved, with why. They stay where they were.
    pub failed: Vec<(PathBuf, String)>,
}

/// Discard every dead owner's staging or discard tree in `repo_dir` (#9239).
///
/// Why: before #9239 the sweep looked only at `.<slot>.seeding.` for the slot
/// being seeded, so orphans under every other index stayed forever. Liveness
/// stays the only safe test: deleting a LIVE run's staging tree would restore
/// the two-writer corruption the unique name removes.
/// What: one `read_dir` of `repo_dir`, then [`discard`] — a rename and a
/// background delete — for each entry whose owner pid is dead, whatever its
/// slot index. A tree whose name does not parse, or whose owner is alive, is
/// left alone (ADR-0045). A rename that fails is reported and the tree stays
/// where it was. Nothing here ever hands a tree to a build.
/// Test: `a_dead_owners_staging_under_another_index_is_swept`,
/// `a_live_owners_staging_is_never_swept`,
/// `a_sweep_that_cannot_move_a_tree_reports_it_and_leaves_it`.
pub fn sweep_abandoned_staging(repo_dir: &Path) -> SweepReport {
    let mut report = SweepReport::default();
    for entry in staging_entries(repo_dir).into_iter().filter(|e| !e.alive) {
        match discard(&entry.path) {
            Ok(done) => report.discarded.push(done),
            Err(err) => {
                tracing::warn!(tree = %entry.path.display(), "could not sweep an abandoned builder-slot tree: {err}");
                report.failed.push((entry.path, err.to_string()));
            }
        }
    }
    report
}

/// One repo's slots and the trees beside them, for `tm build-lease status`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RepoStatus {
    /// `<root>/<owner>/<repo>`.
    pub dir: PathBuf,
    /// Each `slot-<n>` directory and whether its seed has committed.
    pub slots: Vec<(u32, bool)>,
    /// Staging and discard trees, live or abandoned.
    pub staging: Vec<StagingEntry>,
}

/// Every repo under the pool `root`, read-only (#9239).
///
/// What: lists, never creates or deletes. A repo directory that cannot be
/// read shows no slots.
/// Test: `pool_status_lists_a_seed_in_progress_and_slot_states`.
#[must_use]
pub fn pool_status(root: &Path) -> Vec<RepoStatus> {
    let mut repos: Vec<RepoStatus> = repo_dirs(root)
        .into_iter()
        .map(|dir| {
            let mut slots: Vec<(u32, bool)> = std::fs::read_dir(&dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|e| {
                            let name = e.file_name();
                            let index = name.to_str()?.strip_prefix("slot-")?.parse().ok()?;
                            Some((index, marker_reads_seeded(&e.path())))
                        })
                        .collect()
                })
                .unwrap_or_default();
            slots.sort_unstable();
            let staging = staging_entries(&dir);
            RepoStatus {
                dir,
                slots,
                staging,
            }
        })
        .collect();
    repos.sort_by(|a, b| a.dir.cmp(&b.dir));
    repos
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod tests;
