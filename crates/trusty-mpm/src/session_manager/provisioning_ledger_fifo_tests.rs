//! A FIFO where a launch or a decommission reads a provisioning file (#8540).
//!
//! Why: `std::fs::read` on a FIFO blocks until a writer appears, so a FIFO
//! named `CLAUDE.md` or `.gitignore` hung a launch's `snapshot` and the
//! decommission `?? .gitignore` checks forever.
//! What: each test runs the read on a thread and requires an answer inside a
//! bound; a miss wakes the blocked reader so the thread ends, then fails.
//! Test: this file IS the test module.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use super::decommission_force::is_provisioning_entry;
use super::provisioning_ledger::{ProvisioningLedger, snapshot};

/// How long a non-blocking read may take before the test calls it a hang.
const BOUND: Duration = Duration::from_secs(10);

/// Make a FIFO at `path`.
fn mkfifo(path: &Path) {
    let c = CString::new(path.as_os_str().as_bytes()).expect("path has no NUL");
    // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo {}", path.display());
}

/// Run `f` on a thread; `Some` with its answer when it returns inside
/// [`BOUND`]. On a miss, a write-open of each FIFO in `fifos` wakes the
/// blocked reader with EOF so the thread does not outlive the test.
fn answers_in_time<T: Send + 'static>(
    fifos: &[PathBuf],
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    let answer = rx.recv_timeout(BOUND).ok();
    if answer.is_none() {
        for _ in 0..16 {
            for fifo in fifos {
                let _ = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(fifo);
            }
        }
    }
    answer
}

/// 🔴 #8540 follow-up: `pre_state` and the `.gitignore` read blocked on a
/// FIFO, so `snapshot` never returned. Fails (times out) at e911f70771.
#[test]
fn a_fifo_claude_md_or_gitignore_does_not_block_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().to_path_buf();
    let fifos = vec![ws.join("CLAUDE.md"), ws.join(".gitignore")];
    for fifo in &fifos {
        mkfifo(fifo);
    }
    let answered = answers_in_time(&fifos, move || {
        snapshot(&ws);
    });
    assert!(answered.is_some(), "snapshot blocked on a FIFO");
}

/// 🔴 #8540 follow-up: both `?? .gitignore` checks read the file with a
/// blocking read. A FIFO must answer "not tm's" at once, never hang and never
/// be excused. Fails (times out) at e911f70771.
#[test]
fn a_fifo_untracked_gitignore_is_not_excused_and_does_not_block() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir.path().to_path_buf();
    let fifo = ws.join(".gitignore");
    mkfifo(&fifo);
    let ledger = ProvisioningLedger {
        version: 1,
        gitignore_appended: vec![".trusty-mpm/sessions/".to_string()],
        ..ProvisioningLedger::default()
    };
    let fifos = vec![fifo];
    let ws_ledger = ws.clone();
    let by_ledger = answers_in_time(&fifos, move || ledger.excuses(&ws_ledger, "?? .gitignore"));
    assert_eq!(by_ledger, Some(false), "ledger check: hung or excused");
    let by_force = answers_in_time(&fifos, move || {
        is_provisioning_entry(&ws, None, "?? .gitignore")
    });
    assert_eq!(by_force, Some(false), "--force check: hung or excused");
}
