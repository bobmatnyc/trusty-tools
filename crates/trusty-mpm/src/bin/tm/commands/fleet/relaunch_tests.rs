//! Tests for `tm fleet relaunch` (#9092): each contract step and refusal,
//! with tmux, the process table and the launch path stubbed. The full run
//! against a private tmux server is `tests/tm_fleet.rs` (`fleet_relaunch_*`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use trusty_mpm::core::architect_conversation::ConversationStart;
use trusty_mpm::core::architect_launch::ARCHITECT_RECORDS;
use trusty_mpm::core::process::PaneClaude;
use trusty_mpm::core::twin_identity::ArmingRecord;

use super::super::launch::{Launched, PaneState};
use super::super::session_name::SessionNames;
use super::super::{Probe, Step};
use super::*;

/// The PID the stubbed pane's `claude` has; no such process exists.
const OLD_PID: u32 = 3_999_901;

/// The PID the stubbed restart records.
const NEW_PID: u32 = 3_999_902;

thread_local! {
    static PANE: RefCell<PaneState> = const { RefCell::new(PaneState::Absent) };
    static PANE_CLAUDE: RefCell<VecDeque<PaneClaude>> = const { RefCell::new(VecDeque::new()) };
    static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn pane(_: &str) -> PaneState {
    PANE.with(|p| p.borrow().clone())
}

fn no_stamp(_: &str) -> Option<String> {
    None
}

fn old_claude(_: &str) -> Option<u32> {
    Some(OLD_PID)
}

fn architect_ok(_: &Path, _: &str) -> anyhow::Result<()> {
    Ok(())
}

/// The planted answers in order; `Absent` once they run out.
fn pane_claude(_: &str) -> PaneClaude {
    PANE_CLAUDE.with(|q| q.borrow_mut().pop_front().unwrap_or(PaneClaude::Absent))
}

fn stop_ok(pid: u32) -> Result<(), String> {
    CALLS.with(|c| c.borrow_mut().push(format!("stop {pid}")));
    Ok(())
}

fn restart_bound(dir: &Path, _: &Path, _: &str) -> anyhow::Result<Launched> {
    CALLS.with(|c| c.borrow_mut().push("restart".to_owned()));
    let record = ArmingRecord {
        pid: NEW_PID,
        start_time: 1,
        project_dir: dir.to_path_buf(),
        armed_at: String::new(),
    };
    Ok((Ok(record), ConversationStart::Resume("c-1".to_owned())))
}

fn restart_unbound(_: &Path, _: &Path, _: &str) -> anyhow::Result<Launched> {
    CALLS.with(|c| c.borrow_mut().push("restart".to_owned()));
    Ok((
        Err("no `claude` process appeared".to_owned()),
        ConversationStart::Resume("c-1".to_owned()),
    ))
}

const STUB: Ops = Ops {
    probe: Probe {
        pane,
        stamp: no_stamp,
        claude: old_claude,
    },
    architect: architect_ok,
    pane_claude,
    stop: stop_ok,
    restart: restart_bound,
};

/// A scratch home and Architect directory, with the stubs' state planted.
struct Fixture {
    tmp: tempfile::TempDir,
}

impl Fixture {
    fn new(pane: PaneState, pane_claude: &[PaneClaude]) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("arch")).unwrap();
        let fx = Self { tmp };
        // [`live`]'s empty path stands for the fixture's own directory.
        let pane = match pane {
            PaneState::Live(p) if p.as_os_str().is_empty() => PaneState::Live(fx.dir()),
            other => other,
        };
        PANE.with(|p| *p.borrow_mut() = pane);
        PANE_CLAUDE.with(|q| *q.borrow_mut() = pane_claude.iter().copied().collect());
        CALLS.with(|c| c.borrow_mut().clear());
        fx
    }

    fn dir(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path().join("arch")).unwrap()
    }

    fn relaunch(&self, ops: Ops) -> anyhow::Result<RelaunchReport> {
        relaunch(
            &self.dir(),
            self.tmp.path(),
            &SessionNames::default_names(),
            ops,
        )
    }
}

fn calls() -> Vec<String> {
    CALLS.with(|c| c.borrow().clone())
}

fn live() -> PaneState {
    PaneState::Live(PathBuf::new())
}

#[test]
fn relaunch_refuses_an_absent_session_and_names_init() {
    let fx = Fixture::new(PaneState::Absent, &[]);
    let err = fx.relaunch(STUB).expect_err("an absent session is refused");
    let text = format!("{err:#}");
    assert!(text.contains("no tmux session tm-architect runs"), "{text}");
    assert!(text.contains("tm fleet init --dir"), "{text}");
    assert!(calls().is_empty(), "{:?}", calls());
}

/// One refusal case: the pane, the `claude` answers, the PID lookup and the
/// text the error must carry.
type Refusal = (
    PaneState,
    &'static [PaneClaude],
    fn(&str) -> Option<u32>,
    &'static str,
);

#[test]
fn relaunch_refuses_before_any_change_when_the_pane_cannot_be_read() {
    let cases: [Refusal; 5] = [
        (
            PaneState::Unknown("no tmux".to_owned()),
            &[],
            old_claude,
            "cannot read tmux session tm-architect",
        ),
        (PaneState::Dead(PathBuf::new()), &[], old_claude, "is dead"),
        (
            PaneState::Live(PathBuf::from("/elsewhere")),
            &[],
            old_claude,
            "runs in /elsewhere",
        ),
        (
            live(),
            &[PaneClaude::Unknown],
            old_claude,
            "cannot tell whether a `claude`",
        ),
        // A `claude` that is not under the pane shell cannot be stopped.
        (
            live(),
            &[PaneClaude::Present],
            |_| None,
            "not under its shell",
        ),
    ];
    for (i, (pane, answers, claude, want)) in cases.into_iter().enumerate() {
        let fx = Fixture::new(pane, answers);
        let mut ops = STUB;
        ops.probe.claude = claude;
        let err = fx.relaunch(ops).expect_err("refused");
        let text = format!("{err:#}");
        assert!(text.contains(want), "case {i}: {text}");
        assert!(calls().is_empty(), "case {i}: {:?}", calls());
    }
}

#[test]
fn relaunch_refuses_a_directory_that_is_not_an_architect() {
    let fx = Fixture::new(live(), &[PaneClaude::Present]);
    let mut ops = STUB;
    ops.architect = |_, _| anyhow::bail!("a launch would run the PM profile");
    let err = fx.relaunch(ops).expect_err("a PM directory is refused");
    assert!(format!("{err:#}").contains("PM profile"), "{err:#}");
    assert!(calls().is_empty(), "{:?}", calls());
}

#[test]
fn relaunch_stops_the_old_claude_before_the_new_one_starts() {
    let fx = Fixture::new(live(), &[PaneClaude::Present, PaneClaude::Absent]);
    let report = fx.relaunch(STUB).expect("relaunched");
    assert_eq!(calls(), [format!("stop {OLD_PID}"), "restart".to_owned()]);
    assert!(!report.failed(), "{}", report.render());
    let text = report.render();
    assert!(
        text.contains(&format!(
            "stopped the unbound `claude` pid {OLD_PID} in tmux session tm-architect"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("bound to claude pid {NEW_PID}")),
        "{text}"
    );
    assert!(text.contains("resuming conversation c-1"), "{text}");
    let reg = report
        .registration
        .expect("a bound Architect is registered");
    assert_eq!(reg.session, "tm-architect");
    assert_eq!(reg.dir, fx.dir());
}

#[test]
fn relaunch_starts_a_claude_when_none_ran() {
    let fx = Fixture::new(live(), &[PaneClaude::Absent]);
    let report = fx.relaunch(STUB).expect("relaunched");
    assert_eq!(calls(), ["restart".to_owned()]);
    assert!(matches!(&report.steps[0], Step::Unchanged(t) if t.contains("no `claude` ran")));
}

#[test]
fn relaunch_never_types_into_a_pane_that_still_holds_a_claude() {
    let fx = Fixture::new(live(), &[PaneClaude::Present, PaneClaude::Present]);
    let err = fx.relaunch(STUB).expect_err("a surviving claude refuses");
    assert!(
        format!("{err:#}").contains("still holds a `claude`"),
        "{err:#}"
    );
    assert_eq!(calls(), [format!("stop {OLD_PID}")], "no restart");

    let fx = Fixture::new(live(), &[PaneClaude::Present]);
    let mut ops = STUB;
    ops.stop = |_| Err("permission denied".to_owned());
    let err = fx.relaunch(ops).expect_err("a failed stop refuses");
    assert!(
        format!("{err:#}").contains("could not stop claude pid"),
        "{err:#}"
    );
    assert!(calls().is_empty(), "{:?}", calls());
}

#[test]
fn relaunch_reports_an_unrecorded_claude_as_failed() {
    let fx = Fixture::new(live(), &[PaneClaude::Absent]);
    let mut ops = STUB;
    ops.restart = restart_unbound;
    let report = fx.relaunch(ops).expect("the run completes");
    assert!(report.failed());
    assert!(report.registration.is_none());
    let text = report.render();
    assert!(text.contains("FAILED"), "{text}");
    assert!(text.contains("but it is NOT bound"), "{text}");
}

/// Write the `<pid>.architect` record (when `project` is set) and the
/// sidecar naming `session` under `root`.
fn plant(root: &Path, pid: u32, session: &str, project: Option<&Path>) {
    if let Some(project) = project {
        let record = ArmingRecord {
            pid,
            start_time: 50,
            project_dir: project.to_path_buf(),
            armed_at: String::new(),
        };
        ARCHITECT_RECORDS.write(root, &record).expect("record");
    }
    std::fs::write(
        root.join(format!("architect-launch/{pid}.architect-session")),
        format!(r#"{{"pid": {pid}, "start_time": 50, "session": "{session}"}}"#),
    )
    .expect("sidecar");
}

#[test]
fn prune_replaced_removes_only_this_sessions_dead_records() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join(".trusty-mpm");
    std::fs::create_dir_all(root.join("architect-launch")).unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    plant(&root, 100, "tm-architect", Some(&dir)); // dead: removed
    plant(&root, 101, "tm-architect", Some(&dir)); // alive: kept
    plant(&root, 102, "tm-architect", Some(&other)); // other project: kept
    plant(&root, 103, "tm-other", Some(&dir)); // other session: kept
    plant(&root, 104, "tm-architect", Some(&dir)); // PID reused: removed
    plant(&root, 105, "tm-architect", Some(&dir)); // cannot tell: kept
    plant(&root, 106, "tm-architect", None); // orphan sidecar, dead: removed
    plant(&root, NEW_PID, "tm-architect", Some(&dir)); // the new launch: kept
    let start_time = |pid: u32| match pid {
        101 => Ok(Some(50)),
        104 => Ok(Some(51)),
        105 => Err("ps failed".to_owned()),
        _ => Ok(None),
    };

    let step = prune_replaced(&root, &dir, "tm-architect", NEW_PID, &start_time);

    let Some(Step::Changed(text)) = step else {
        panic!("expected a removal, got {step:?}");
    };
    for pid in [100, 104, 106] {
        assert!(text.contains(&pid.to_string()), "{text}");
        assert!(!ARCHITECT_RECORDS.path(&root, pid).exists(), "record {pid}");
        let sidecar = root.join(format!("architect-launch/{pid}.architect-session"));
        assert!(!sidecar.exists(), "sidecar {pid}");
    }
    for pid in [101, 102, 103, 105, NEW_PID] {
        assert!(ARCHITECT_RECORDS.path(&root, pid).exists(), "record {pid}");
        let sidecar = root.join(format!("architect-launch/{pid}.architect-session"));
        assert!(sidecar.exists(), "sidecar {pid}");
    }
    // Nothing left to remove: no step.
    assert_eq!(
        prune_replaced(&root, &dir, "tm-architect", NEW_PID, &start_time),
        None
    );
}

/// Spawn `name` (a symlink to `/bin/sleep` in `bin`) for 600 s, reaped by a
/// thread so a stopped child does not linger as a zombie.
fn spawn_sleeper(bin: &Path, name: &str) -> u32 {
    let path = bin.join(name);
    std::os::unix::fs::symlink("/bin/sleep", &path).expect("symlink");
    let mut child = std::process::Command::new(&path)
        .arg("600")
        .spawn()
        .expect("spawn");
    let pid = child.id();
    std::thread::spawn(move || child.wait());
    pid
}

#[test]
fn stop_claude_ends_a_claude_process() {
    let bin = tempfile::tempdir().expect("tempdir");
    let pid = spawn_sleeper(bin.path(), "claude-sleep");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !process::process_name_is_claude(pid) && Instant::now() < deadline {
        std::thread::sleep(STOP_POLL);
    }
    stop_claude(pid).expect("stopped");
    assert!(
        process_facts(pid).is_err() || !process::process_name_is_claude(pid),
        "pid {pid} still runs"
    );
}

#[test]
fn stop_claude_refuses_a_process_not_named_claude() {
    let bin = tempfile::tempdir().expect("tempdir");
    let pid = spawn_sleeper(bin.path(), "sleeper");
    let err = stop_claude(pid).expect_err("not a claude");
    assert!(err.contains("is not a `claude` process"), "{err}");
    assert!(process::is_process_alive(pid), "the process was signalled");
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
}
