//! Tests for the sweep's git ceiling (#8306).
//!
//! Why: the orphan sweep ran every git call with no timeout, so one wedged
//! `git` stalled the whole pass and a hung check could only ever resolve by
//! waiting. These tests wedge REAL git deterministically — no timer decides
//! when git hangs, and none decides when it is released:
//! - [`wedge_every_git_in`] replaces the worktree's `HEAD` with a FIFO. git
//!   opens `HEAD` while discovering the repository, and `open` on a FIFO blocks
//!   until a writer appears, so every git command run in that worktree hangs.
//! - [`wedge_status_in`] points the worktree's own `core.fsmonitor` at a hook
//!   that blocks reading a FIFO, so only `git status` in that worktree hangs.
//!
//! Each gate releases its FIFO on drop, so a run against pre-fix code (where
//! nothing kills the hung git) does not leave processes blocked forever.
//! What: a wedged git is killed within the ceiling, a timed-out check keeps the
//! worktree, and a sweep with one wedged candidate still reclaims the next.
//! Test: this file IS the test module.

#![cfg(unix)]

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::{git_ceiling, is_timed_out, with_git_ceiling};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_safety::{
    DirtyWorktreePolicy, git_worktree_list_agrees, inspect_dirt,
};

/// The ceiling the tests run git under: long enough that a healthy git on a
/// loaded machine finishes, short enough to keep the suite quick.
const CEILING: Duration = Duration::from_secs(5);

/// How long a test waits for the code under test to return at all. Pre-fix,
/// a wedged git never returns, and this is where that shows up as a failure.
const WATCHDOG: Duration = Duration::from_secs(60);

/// Run `f` on its own thread; fail the test if it has not returned by [`WATCHDOG`].
fn within_watchdog<T: Send + 'static>(label: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(WATCHDOG).unwrap_or_else(|_| {
        panic!("{label} did not return within {WATCHDOG:?}: a wedged git call is unbounded (#8306)")
    })
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn mkfifo(path: &Path) {
    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo {}", path.display());
}

/// Is some process holding `fifo` open (or blocked opening it) for reading?
///
/// A non-blocking write-open of a FIFO fails with ENXIO exactly when there is
/// no reader, so after a git that was blocked on `fifo` is killed AND reaped,
/// this is `false` with no waiting.
fn has_reader(fifo: &Path) -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fifo)
        .is_ok()
}

/// Wake every reader blocked on `fifo`: each write-open lets a blocked open
/// complete, and the close hands the reader EOF.
fn release(fifo: &Path) {
    for _ in 0..64 {
        if !has_reader(fifo) {
            return;
        }
    }
}

/// A FIFO standing in for a worktree's `HEAD`; restores the real file on drop.
struct HeadWedge {
    fifo: PathBuf,
    head: Vec<u8>,
}

impl Drop for HeadWedge {
    fn drop(&mut self) {
        let staged = self.fifo.with_extension("restore");
        let _ = std::fs::write(&staged, &self.head);
        // Rename first so no new git opens the FIFO, then wake the old readers.
        let held = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.fifo);
        let _ = std::fs::rename(&staged, &self.fifo);
        drop(held);
    }
}

/// Make every git command run inside `wt` block in `open(HEAD)`.
fn wedge_every_git_in(wt: &Path) -> HeadWedge {
    let gitdir = PathBuf::from(git(wt, &["rev-parse", "--absolute-git-dir"]));
    let fifo = gitdir.join("HEAD");
    let head = std::fs::read(&fifo).expect("read HEAD");
    std::fs::remove_file(&fifo).expect("remove HEAD");
    mkfifo(&fifo);
    HeadWedge { fifo, head }
}

/// A per-worktree fsmonitor hook blocked on a FIFO; disarmed on drop.
struct StatusWedge {
    hook: PathBuf,
    gate: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for StatusWedge {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.hook, "#!/bin/sh\nexit 1\n");
        release(&self.gate);
    }
}

/// Make `git status` — and only `git status` — inside `wt` hang.
fn wedge_status_in(repo: &Path, wt: &Path) -> StatusWedge {
    let dir = tempfile::tempdir().expect("tempdir");
    let gate = dir.path().join("gate");
    mkfifo(&gate);
    let hook = dir.path().join("fsmonitor-hook");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\nexec cat \"{}\" >/dev/null\n", gate.display()),
    )
    .expect("write hook");
    git(repo, &["config", "extensions.worktreeConfig", "true"]);
    git(
        wt,
        &[
            "config",
            "--worktree",
            "core.fsmonitor",
            &hook.to_string_lossy(),
        ],
    );
    let status = Command::new("chmod").arg("+x").arg(&hook).status();
    assert!(status.is_ok_and(|s| s.success()), "chmod the hook");
    StatusWedge {
        hook,
        gate,
        _dir: dir,
    }
}

/// The ceiling applies inside the closure only, and survives a panic there.
#[test]
fn with_git_ceiling_scopes_and_restores() {
    let outer = git_ceiling();
    assert_eq!(
        with_git_ceiling(Duration::from_millis(7), git_ceiling),
        Duration::from_millis(7)
    );
    assert_eq!(git_ceiling(), outer);
    let panicked = std::panic::catch_unwind(|| {
        with_git_ceiling(Duration::from_millis(9), || panic!("inside the ceiling"))
    });
    assert!(panicked.is_err());
    assert_eq!(git_ceiling(), outer, "a panic must not leak the ceiling");
}

/// 🔴 #8306 REGRESSION: a git that never answers is killed at the ceiling, and
/// the dirty check reports the tree DIRTY — never clean, never "not a worktree".
#[test]
fn a_wedged_git_is_killed_within_the_ceiling() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("wedged");
    let wedge = wedge_every_git_in(&wt);

    let started = Instant::now();
    let probe = wt.clone();
    let dirt = within_watchdog("inspect_dirt", move || {
        with_git_ceiling(CEILING, || inspect_dirt(&probe))
    });
    let elapsed = started.elapsed();

    let dirt = dirt.expect("a worktree whose git never answered must be reported dirty");
    assert!(is_timed_out(&dirt.reason), "reason: {}", dirt.reason);
    assert!(
        elapsed >= CEILING,
        "returned before the ceiling: {elapsed:?}"
    );
    assert!(
        !has_reader(&wedge.fifo),
        "the timed-out git is still blocked on HEAD: it was not killed"
    );
}

/// 🔴 #8306 REGRESSION: `git worktree list` timing out is "unknown", which
/// disagrees and keeps the tree. It must never fall into the "unanswerable,
/// so agree" arm that lets a deletion proceed.
#[test]
fn a_timed_out_worktree_list_disagrees() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("wedged-list");
    assert!(
        git_worktree_list_agrees(&wt),
        "control: a healthy tree agrees"
    );
    let wedge = wedge_every_git_in(&wt);

    let probe = wt.clone();
    let agrees = within_watchdog("git_worktree_list_agrees", move || {
        with_git_ceiling(CEILING, || git_worktree_list_agrees(&probe))
    });

    assert!(!agrees, "a timed-out probe must disagree, keeping the tree");
    assert!(!has_reader(&wedge.fifo), "the timed-out git was not killed");
}

/// 🔴 #8306 REGRESSION: one wedged candidate costs the sweep one ceiling, not
/// the pass. The wedged tree sorts FIRST, so the clean tree behind it is
/// reclaimed only if the sweep got past the hang.
#[test]
fn a_sweep_keeps_a_wedged_tree_and_reclaims_the_next() {
    let fx = GitWorktreeFixture::new();
    let wedged = fx.add_worktree("aaa-wedged");
    let clean = fx.add_worktree("zzz-clean");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wedged);
    GitWorktreeFixture::stamp_reclaimable_sentinel(&clean);
    let _wedge = wedge_status_in(&fx.repo, &wedged);

    let repos_root = fx.repos_root.clone();
    let outcome = within_watchdog("the orphan sweep", move || {
        with_git_ceiling(CEILING, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async {
                let store = tempfile::tempdir().expect("tempdir");
                let mgr = crate::session_manager::SessionManager::new(
                    store.path(),
                    crate::session_manager::tests::FakeTmuxDriver::new(),
                )
                .await
                .expect("manager");
                mgr.prune_orphaned_worktrees(
                    &repos_root,
                    &[],
                    false,
                    DirtyWorktreePolicy::Skip,
                    &[],
                )
                .await
                .expect("prune must not error")
            })
        })
    });

    assert_eq!(outcome.removed, vec![clean.clone()], "{outcome:?}");
    assert!(
        !clean.exists(),
        "the clean tree behind the hang is reclaimed"
    );
    assert!(wedged.exists(), "the tree whose check timed out is kept");
    let kept = outcome
        .skipped_dirty
        .iter()
        .find(|d| d.path == wedged)
        .unwrap_or_else(|| panic!("the wedged tree is reported as kept: {outcome:?}"));
    assert!(is_timed_out(&kept.reason), "reason: {}", kept.reason);
}
