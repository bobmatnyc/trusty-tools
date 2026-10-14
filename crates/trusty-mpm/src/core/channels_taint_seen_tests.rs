//! Tests for the #9877 seen-id handshake between [`reapply`] (SessionStart)
//! and [`taint_managed_session`] (the inbound deliverer).
//!
//! Why: the handshake is what keeps a delivered answer from reaching a Claude
//! id that was never tainted when a `SessionEnd` was lost or raced. Each test
//! goes red against a reapply that does not record the id before it reads the
//! marker, a deliverer that does not taint the recorded ids after it writes
//! the marker, or a failure on either side that is not an `Err`.
//! What: a temp taint directory; [`at_handshake_gap`] forces the interleaving.
//! Test: this is the test module.

use std::cell::Cell;
use std::os::unix::fs::PermissionsExt as _;
use std::rc::Rc;

use super::*;

const ASKER: &str = "m-asker";
/// The id the managed record still names: its `SessionEnd` was lost.
const X: &str = "claude-x";
/// The id the pane runs after `/clear`.
const Y: &str = "claude-y";

fn mark() -> TaintMark {
    TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: Utc::now(),
        workspace: PathBuf::from("/"),
    }
}

fn dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = taint_dir(tmp.path());
    (tmp, dir)
}

/// The asker's seen-id list.
fn seen_file(dir: &Path) -> PathBuf {
    dir.join("managed").join(format!("{ASKER}.seen"))
}

/// Set `path`'s mode; each test restores it before its tempdir drops.
fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// The order SessionStart(Y) and the deliverer's taint of X run in.
#[derive(Debug, Clone, Copy)]
enum Schedule {
    /// The issue's race: reapply(Y) completes, then the marker write.
    ReapplyFirst,
    /// The deliverer runs whole between reapply's record and its marker read.
    DeliverInsideReapply,
    /// Reapply runs whole between the deliverer's marker write and its read.
    ReapplyInsideDeliver,
}

/// Whether `Y` reads tainted after the two sides ran in `schedule`, and
/// whether a forced interleaving actually ran inside the other side.
fn y_tainted_after(schedule: Schedule) -> (bool, bool) {
    let (_tmp, dir) = dir();
    let fired = Rc::new(Cell::new(false));
    let flag = Rc::clone(&fired);
    let other = dir.clone();
    match schedule {
        Schedule::ReapplyFirst => {
            reapply(&dir, ASKER, Y).expect("reapply");
            taint_managed_session(&dir, ASKER, X, &mark()).expect("deliver");
            fired.set(true);
        }
        Schedule::DeliverInsideReapply => {
            at_handshake_gap(move || {
                flag.set(true);
                taint_managed_session(&other, ASKER, X, &mark()).expect("deliver");
            });
            reapply(&dir, ASKER, Y).expect("reapply");
        }
        Schedule::ReapplyInsideDeliver => {
            at_handshake_gap(move || {
                flag.set(true);
                reapply(&other, ASKER, Y).expect("reapply");
            });
            taint_managed_session(&dir, ASKER, X, &mark()).expect("deliver");
        }
    }
    (read_state(&dir, Y).is_tainted(), fired.get())
}

/// 🔴 #9877 (closure 2): reapply(Y) runs before the deliverer's marker write
/// and the deliverer's re-read still sees X. Y must read tainted in that
/// order and in both forced interleavings, so reversing either side's two
/// steps leaves one schedule untainted.
#[test]
fn reapply_before_the_marker_write_still_taints_the_new_id() {
    let mut untainted = Vec::new();
    for schedule in [
        Schedule::ReapplyFirst,
        Schedule::DeliverInsideReapply,
        Schedule::ReapplyInsideDeliver,
    ] {
        let (tainted, fired) = y_tainted_after(schedule);
        assert!(fired, "{schedule:?}: the interleaving never ran");
        if !tainted {
            untainted.push(schedule);
        }
    }
    assert!(
        untainted.is_empty(),
        "Y reads untainted after {untainted:?}; the answer would reach an untainted pane"
    );
}

/// 🔴 #9877 (Fail-Open Check): a SessionStart whose id cannot be recorded
/// fails, even for a session with no mark, since no later delivery can see it.
#[test]
fn a_seen_record_that_cannot_be_written_fails_the_reapply() {
    let (_tmp, dir) = dir();
    let managed = dir.join("managed");
    std::fs::create_dir_all(&managed).expect("mkdir");
    chmod(&managed, 0o500);
    let result = reapply(&dir, ASKER, Y);
    chmod(&managed, 0o700);
    assert!(result.is_err(), "an unrecorded id read as fine: {result:?}");
}

/// 🔴 #9877 (Fail-Open Check): a seen list the deliverer cannot read or
/// parse fails the taint, so the delivery is refused.
#[test]
fn a_seen_list_that_cannot_be_read_fails_the_taint() {
    let (_tmp, dir) = dir();
    std::fs::create_dir_all(dir.join("managed")).expect("mkdir");
    std::fs::write(seen_file(&dir), format!("{Y}\n")).expect("seen");
    chmod(&seen_file(&dir), 0o000);
    let unreadable = taint_managed_session(&dir, ASKER, X, &mark());
    chmod(&seen_file(&dir), 0o600);
    assert!(unreadable.is_err(), "unreadable: {unreadable:?}");

    std::fs::write(seen_file(&dir), "../escape\n").expect("seen");
    let malformed = taint_managed_session(&dir, ASKER, X, &mark());
    assert!(malformed.is_err(), "malformed: {malformed:?}");
}

/// 🔴 #9877 (Fail-Open Check): a seen id whose taint cannot be written fails
/// the taint; the deliverer never sends to an id it could not taint.
#[test]
fn a_seen_id_whose_taint_cannot_be_written_fails_the_taint() {
    let (_tmp, dir) = dir();
    std::fs::create_dir_all(dir.join("managed")).expect("mkdir");
    std::fs::write(seen_file(&dir), format!("{Y}\n")).expect("seen");
    let top = dir.clone();
    at_handshake_gap(move || chmod(&top, 0o500));
    let result = taint_managed_session(&dir, ASKER, X, &mark());
    chmod(&dir, 0o700);
    assert!(result.is_err(), "Y left untainted read as fine: {result:?}");
    assert_eq!(read_state(&dir, Y), TaintState::Untainted, "precondition");
}

/// 🔴 #9877: the seen list is not a taint, so it never raises presence, and
/// a retire removes it with the mark.
#[test]
fn the_seen_list_is_no_taint_and_a_retire_removes_it() {
    let (_tmp, dir) = dir();
    reapply(&dir, ASKER, Y).expect("reapply");
    assert!(seen_file(&dir).exists(), "SessionStart recorded no id");
    assert_eq!(presence(&dir), Presence::None, "a seen list alone");
    taint_managed_session(&dir, ASKER, X, &mark()).expect("taint");
    retire(&dir, ASKER).expect("retire");
    assert!(!seen_file(&dir).exists(), "the retire left the seen list");
    assert_eq!(presence(&dir), Presence::None, "retired");
}

/// 🔴 #9877 (Fail-Open Check): a seen list the retire cannot remove fails
/// the retire, and the mark stays.
#[test]
fn a_seen_list_a_retire_cannot_remove_keeps_the_mark() {
    let (_tmp, dir) = dir();
    taint_managed_session(&dir, ASKER, X, &mark()).expect("taint");
    std::fs::create_dir_all(seen_file(&dir).join("blocker")).expect("block");
    let result = retire(&dir, ASKER);
    assert!(result.is_err(), "the retire read as complete: {result:?}");
    assert!(
        managed_mark(&dir, ASKER).expect("read").is_some(),
        "the mark is kept"
    );
}
