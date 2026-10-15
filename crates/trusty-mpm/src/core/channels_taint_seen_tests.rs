//! Tests for the #9877 seen-id handshake between [`reapply`] (SessionStart)
//! and [`taint_managed_session`] (the inbound deliverer).
//!
//! Why: the handshake is what keeps a delivered answer from reaching a Claude
//! id that was never tainted when a `SessionEnd` was lost or raced. Each test
//! goes red against a reapply that does not record the id before it reads the
//! marker, a deliverer that does not taint the recorded ids after it writes
//! the marker, or a failure on either side that is not an `Err`. #9923: the
//! residual, an id no SessionStart reported, is closed by [`taint_covered`].
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

/// A canonical root holding `ws`, `ws/sub` and `out`, with [`ASKER`] marked
/// for `ws` and [`X`] tainted (#9923); the root, the taint dir and `ws`.
fn scoped() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    for sub in ["ws/sub", "out"] {
        std::fs::create_dir_all(root.join(sub)).expect("mkdir");
    }
    let dir = taint_dir(&root.join("home"));
    let ws_mark = TaintMark {
        workspace: root.join("ws"),
        ..mark()
    };
    taint_managed_session(&dir, ASKER, X, &ws_mark).expect("taint");
    (tmp, dir, root)
}

/// 🔴 #9923: an id no SessionStart reported is tainted for the mark covering
/// its launch directory (or, second, its live cwd), and that mark's retire
/// removes it; an id no mark covers is left alone.
#[test]
fn a_hook_less_id_is_tainted_for_the_mark_covering_its_launch_dir() {
    let (_tmp, dir, root) = scoped();
    let (ws, out) = (root.join("ws"), root.join("out"));
    let covered = |id, launch: Option<&Path>, cwd: Option<&Path>| {
        taint_covered(&dir, id, launch, cwd).expect("taint_covered")
    };
    assert_eq!(covered(Y, Some(&out), Some(&out)), None, "outside");
    assert_eq!(covered(Y, Some(&out), None), None, "outside, no cwd");
    assert_eq!(read_state(&dir, Y), TaintState::Untainted);
    let asker = Some(ASKER.to_owned());
    assert_eq!(covered(Y, Some(&ws), Some(&out)), asker, "launched in ws");
    assert_eq!(
        covered("y-sub", Some(&ws.join("sub")), None),
        asker,
        "under"
    );
    assert_eq!(covered("y-cwd", Some(&out), Some(&ws)), asker, "cwd in ws");
    assert_eq!(covered("y-none", None, Some(&out)), asker, "no launch dir");
    let record: TaintRecord =
        serde_json::from_slice(&std::fs::read(taint_path(&dir, Y).expect("path")).expect("read"))
            .expect("record");
    assert_eq!(record.managed_session, ASKER);
    retire(&dir, ASKER).expect("retire");
    for id in [Y, "y-sub", "y-cwd", "y-none"] {
        assert_eq!(read_state(&dir, id), TaintState::Untainted, "{id}");
    }
    let bare = taint_dir(&root.join("no-marks"));
    assert_eq!(taint_covered(&bare, Y, None, None).expect("no marks"), None);
}

/// 🔴 #9923 (Fail-Open Check): a bad id, marks that cannot be listed, a mark
/// that cannot be read and a failed taint write are each an `Err`.
#[test]
fn lazy_taint_fails_closed_on_every_error() {
    let (_tmp, dir, root) = scoped();
    let ws = root.join("ws");
    assert!(
        taint_covered(&dir, "../y", Some(&ws), None).is_err(),
        "bad id"
    );

    let managed = dir.join("managed");
    chmod(&managed, 0o000);
    let unlisted = taint_covered(&dir, Y, Some(&ws), None);
    chmod(&managed, 0o700);
    assert!(unlisted.is_err(), "unlistable marks: {unlisted:?}");

    chmod(&dir, 0o500);
    let unwritten = taint_covered(&dir, Y, Some(&ws), None);
    chmod(&dir, 0o700);
    assert!(unwritten.is_err(), "failed write: {unwritten:?}");
    assert_eq!(read_state(&dir, Y), TaintState::Untainted);

    std::fs::write(managed.join("m-bad.json"), b"not json").expect("write");
    let out = root.join("out");
    let unread = taint_covered(&dir, Y, Some(&out), None);
    assert!(unread.is_err(), "unreadable mark: {unread:?}");
}
