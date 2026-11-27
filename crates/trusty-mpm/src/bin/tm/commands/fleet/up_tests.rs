//! Tests for `tm architect up` (#11364): each state-table row against a fake
//! tmux, the lock, and the never-kill and inside-Claude-Code arms.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use trusty_mpm::core::process::PaneClaude;

use super::super::launch::PaneState;
use super::super::session_name::SessionNames;
use super::super::{Probe, Step};
use super::*;

thread_local! {
    static PANE: RefCell<PaneState> = const { RefCell::new(PaneState::Absent) };
    static CLAUDE: RefCell<PaneClaude> = const { RefCell::new(PaneClaude::Absent) };
    static BOUND: RefCell<bool> = const { RefCell::new(false) };
    static RUNS: RefCell<Vec<Action>> = const { RefCell::new(Vec::new()) };
    // #11368: every deploy's mode, in call order, and what the next one returns.
    static DEPLOYS: RefCell<Vec<Mode>> = const { RefCell::new(Vec::new()) };
    static DEPLOY_FAILS: RefCell<Option<bool>> = const { RefCell::new(None) };
    static ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn pane(_: &str) -> PaneState {
    PANE.with(|p| p.borrow().clone())
}
fn no_stamp(_: &str) -> Option<String> {
    None
}
fn no_pid(_: &str) -> Option<u32> {
    None
}
fn claude(_: &str) -> PaneClaude {
    CLAUDE.with(|c| *c.borrow())
}
fn bound(_: &Path, _: &Path, _: Probe, _: &str) -> Result<String, String> {
    if BOUND.with(|b| *b.borrow()) {
        Ok("bound".into())
    } else {
        Err("no launch record".into())
    }
}
/// #11368: records the mode; `DEPLOY_FAILS` `Some(true)` is `Err`, `Some(false)` a failed step.
fn fake_deploy(_: &Path, _: &SessionNames, mode: Mode) -> anyhow::Result<Vec<Step>> {
    DEPLOYS.with(|d| d.borrow_mut().push(mode));
    ORDER.with(|o| o.borrow_mut().push("deploy"));
    match DEPLOY_FAILS.with(|f| *f.borrow()) {
        Some(true) => anyhow::bail!("cannot write scripts/fleet-poll.py"),
        Some(false) => Ok(vec![Step::Failed("poller restart: tmux died".into())]),
        None => Ok(vec![Step::Changed("deployed".into())]),
    }
}
fn record(action: Action, _: &Path, _: &Path, _: &SessionNames) -> anyhow::Result<Outcome> {
    RUNS.with(|r| r.borrow_mut().push(action));
    ORDER.with(|o| o.borrow_mut().push("run"));
    Ok(Outcome {
        steps: vec![Step::Changed(format!("{action:?}"))],
        registration: None,
    })
}

const FAKE: Ops = Ops {
    probe: Probe {
        pane,
        stamp: no_stamp,
        claude: no_pid,
    },
    pane_claude: claude,
    bound,
    run: record,
    deploy: fake_deploy,
};

/// A scratch home with the Architect directory `home/arch`.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("arch")).expect("arch");
    let dir = std::fs::canonicalize(tmp.path().join("arch")).expect("canonical");
    RUNS.with(|r| r.borrow_mut().clear());
    DEPLOYS.with(|d| d.borrow_mut().clear());
    DEPLOY_FAILS.with(|f| *f.borrow_mut() = None);
    ORDER.with(|o| o.borrow_mut().clear());
    (tmp, dir)
}

fn set(p: PaneState, c: PaneClaude, b: bool) {
    PANE.with(|x| *x.borrow_mut() = p);
    CLAUDE.with(|x| *x.borrow_mut() = c);
    BOUND.with(|x| *x.borrow_mut() = b);
}

fn runs() -> Vec<Action> {
    RUNS.with(|r| r.borrow().clone())
}

#[test]
fn each_state_table_row_maps_to_its_action() {
    let (tmp, dir) = fixture();
    let names = SessionNames::default_names();
    let other = PathBuf::from("/elsewhere");
    let rows = [
        (
            PaneState::Absent,
            PaneClaude::Absent,
            false,
            Action::Init,
            0,
        ),
        (
            PaneState::Dead(dir.clone()),
            PaneClaude::Absent,
            false,
            Action::RespawnAndLaunch,
            0,
        ),
        (
            PaneState::Live(dir.clone()),
            PaneClaude::Absent,
            false,
            Action::Launch,
            0,
        ),
        (
            PaneState::Live(dir.clone()),
            PaneClaude::Present,
            true,
            Action::Ensure,
            0,
        ),
        (
            PaneState::Live(dir.clone()),
            PaneClaude::Present,
            false,
            Action::Report,
            0,
        ),
        (
            PaneState::Live(other.clone()),
            PaneClaude::Present,
            true,
            Action::Refuse,
            2,
        ),
        (
            PaneState::Dead(other),
            PaneClaude::Absent,
            false,
            Action::Refuse,
            2,
        ),
        (
            PaneState::Unknown("tmux died".into()),
            PaneClaude::Absent,
            false,
            Action::Fail,
            1,
        ),
        (
            PaneState::Live(dir.clone()),
            PaneClaude::Unknown,
            false,
            Action::Fail,
            1,
        ),
    ];
    for (p, c, b, want, exit) in rows {
        set(p.clone(), c, b);
        RUNS.with(|r| r.borrow_mut().clear());
        let r = up(&dir, tmp.path(), &names, false, false, FAKE);
        assert_eq!(
            (r.action, r.exit),
            (want, exit),
            "{p:?} {c:?} bound={b}: {r:?}"
        );
        let ran = !matches!(want, Action::Report | Action::Refuse | Action::Fail);
        assert_eq!(runs(), if ran { vec![want] } else { vec![] }, "{p:?}");
    }
}

#[test]
fn up_never_kills_a_live_unbound_claude() {
    let (tmp, dir) = fixture();
    set(PaneState::Live(dir.clone()), PaneClaude::Present, false);
    let r = up(
        &dir,
        tmp.path(),
        &SessionNames::default_names(),
        false,
        false,
        FAKE,
    );
    assert_eq!((r.action, r.exit), (Action::Report, 0));
    assert!(runs().is_empty(), "nothing may run: {:?}", runs());
    assert!(
        r.render().contains("unbound: guard exemptions are off"),
        "{}",
        r.render()
    );
}

#[test]
fn inside_claude_code_up_never_starts_or_stops_claude() {
    let (tmp, dir) = fixture();
    let names = SessionNames::default_names();
    for (p, c, b) in [
        (PaneState::Absent, PaneClaude::Absent, false),
        (PaneState::Dead(dir.clone()), PaneClaude::Absent, false),
        (PaneState::Live(dir.clone()), PaneClaude::Absent, false),
        (PaneState::Live(dir.clone()), PaneClaude::Present, false),
    ] {
        set(p.clone(), c, b);
        let r = up(&dir, tmp.path(), &names, true, false, FAKE);
        assert_eq!((r.action, r.exit), (Action::Report, 1), "{p:?}");
    }
    assert!(runs().is_empty(), "{:?}", runs());
    set(PaneState::Live(dir.clone()), PaneClaude::Present, true);
    let r = up(&dir, tmp.path(), &names, true, false, FAKE);
    assert_eq!(
        (r.action, r.exit, runs()),
        (Action::Ensure, 0, vec![Action::Ensure])
    );
}

#[test]
fn dry_run_changes_nothing() {
    let (tmp, dir) = fixture();
    set(PaneState::Absent, PaneClaude::Absent, false);
    let r = up(
        &dir,
        tmp.path(),
        &SessionNames::default_names(),
        false,
        true,
        FAKE,
    );
    assert_eq!(r.action, Action::Init);
    assert!(runs().is_empty());
    assert!(!dir.join(UP_LOCK).exists(), "a dry run takes no lock");
}

// --- the lock: shared across threads, so plain statics ---

static RACE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static RACE_STARTED: AtomicUsize = AtomicUsize::new(0);
static RACE_INITS: AtomicUsize = AtomicUsize::new(0);

fn race_pane(_: &str) -> PaneState {
    let dir = RACE_DIR.lock().expect("dir").clone().expect("set");
    if RACE_STARTED.load(Ordering::SeqCst) > 0 {
        PaneState::Live(dir)
    } else {
        PaneState::Absent
    }
}
fn race_claude(_: &str) -> PaneClaude {
    PaneClaude::Present
}
fn race_bound(_: &Path, _: &Path, _: Probe, _: &str) -> Result<String, String> {
    Ok("bound".into())
}
fn race_run(action: Action, _: &Path, _: &Path, _: &SessionNames) -> anyhow::Result<Outcome> {
    if action == Action::Init {
        RACE_INITS.fetch_add(1, Ordering::SeqCst);
        // Slow enough that an unlocked second `up` reads the session absent too.
        std::thread::sleep(std::time::Duration::from_millis(400));
        RACE_STARTED.fetch_add(1, Ordering::SeqCst);
    }
    Ok(Outcome::default())
}

#[test]
fn two_concurrent_ups_start_one_architect() {
    let (tmp, dir) = fixture();
    *RACE_DIR.lock().expect("dir") = Some(dir.clone());
    let ops = Ops {
        probe: Probe {
            pane: race_pane,
            stamp: no_stamp,
            claude: no_pid,
        },
        pane_claude: race_claude,
        bound: race_bound,
        run: race_run,
        deploy: |_, _, _| Ok(Vec::new()),
    };
    let home = tmp.path().to_owned();
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (dir, home) = (dir.clone(), home.clone());
            std::thread::spawn(move || {
                up(
                    &dir,
                    &home,
                    &SessionNames::default_names(),
                    false,
                    false,
                    ops,
                )
                .action
            })
        })
        .collect();
    let mut actions: Vec<Action> = handles
        .into_iter()
        .map(|h| h.join().expect("join"))
        .collect();
    actions.sort_by_key(|a| format!("{a:?}"));
    assert_eq!(
        RACE_INITS.load(Ordering::SeqCst),
        1,
        "one Architect: {actions:?}"
    );
    assert_eq!(actions, vec![Action::Ensure, Action::Init]);
}

// --- #11368: the bundle deploy before every launch ---

/// The Architect pane state for each launching action, over `dir`.
fn launching_rows(dir: &Path) -> [(PaneState, PaneClaude, bool, Action); 4] {
    [
        (PaneState::Absent, PaneClaude::Absent, false, Action::Init),
        (
            PaneState::Dead(dir.to_owned()),
            PaneClaude::Absent,
            false,
            Action::RespawnAndLaunch,
        ),
        (
            PaneState::Live(dir.to_owned()),
            PaneClaude::Absent,
            false,
            Action::Launch,
        ),
        (
            PaneState::Live(dir.to_owned()),
            PaneClaude::Present,
            true,
            Action::Ensure,
        ),
    ]
}

fn deploys() -> Vec<Mode> {
    DEPLOYS.with(|d| d.borrow().clone())
}

/// #11368: Launch and RespawnAndLaunch deploy too, and every deploy runs
/// before the action that starts `claude`.
#[test]
fn every_launching_action_deploys_before_it_runs_11368() {
    let live = Mode {
        dry_run: false,
        restart: true,
    };
    for row in 0..4 {
        let (tmp, dir) = fixture();
        let (p, c, b, want) = launching_rows(&dir)[row].clone();
        set(p, c, b);
        let r = up(
            &dir,
            tmp.path(),
            &SessionNames::default_names(),
            false,
            false,
            FAKE,
        );
        assert_eq!((r.action, r.exit), (want, 0), "{:?}", r.steps);
        assert_eq!(
            ORDER.with(|o| o.borrow().clone()),
            ["deploy", "run"],
            "{want:?}"
        );
        assert_eq!(deploys(), [live], "{want:?}");
    }
}

/// #11368 Q8: a deploy that fails, or reports a failed step, launches
/// nothing and exits 1, on every launching action.
#[test]
fn a_failed_deploy_launches_nothing_and_exits_1_11368() {
    for fails in [true, false] {
        for row in 0..4 {
            let (tmp, dir) = fixture();
            DEPLOY_FAILS.with(|f| *f.borrow_mut() = Some(fails));
            let (p, c, b, want) = launching_rows(&dir)[row].clone();
            set(p, c, b);
            let r = up(
                &dir,
                tmp.path(),
                &SessionNames::default_names(),
                false,
                false,
                FAKE,
            );
            assert_eq!((r.action, r.exit), (want, 1), "{:?}", r.steps);
            assert!(runs().is_empty(), "{want:?} ran after a failed deploy");
            // #11961: the verdict line names the failed step's own text.
            let why = if fails {
                "(cannot write scripts/fleet-poll.py)"
            } else {
                "(poller restart: tmux died)"
            };
            assert!(
                r.steps
                    .iter()
                    .any(|s| s.contains("nothing was launched") && s.contains(why)),
                "{:?}",
                r.steps
            );
        }
    }
}

/// #11368 Q7: `--dry-run` asks for the planned deploy and runs nothing.
#[test]
fn dry_run_reports_the_planned_deploy_and_runs_nothing_11368() {
    let (tmp, dir) = fixture();
    set(PaneState::Live(dir.clone()), PaneClaude::Absent, false);
    let r = up(
        &dir,
        tmp.path(),
        &SessionNames::default_names(),
        false,
        true,
        FAKE,
    );
    let dry = Mode {
        dry_run: true,
        restart: true,
    };
    assert_eq!(
        (r.action, r.exit, deploys()),
        (Action::Launch, 0, vec![dry])
    );
    assert!(runs().is_empty());
    assert!(!dir.join(UP_LOCK).exists(), "a dry run takes no lock");
}

/// #11368 Q7: inside Claude Code the deploy runs with restarts off; a state
/// `up` only reports deploys nothing.
#[test]
fn inside_claude_code_deploys_files_only_11368() {
    let (tmp, dir) = fixture();
    let names = SessionNames::default_names();
    set(PaneState::Live(dir.clone()), PaneClaude::Absent, false);
    up(&dir, tmp.path(), &names, true, false, FAKE);
    assert!(deploys().is_empty(), "a reported state deploys nothing");
    set(PaneState::Live(dir.clone()), PaneClaude::Present, true);
    let r = up(&dir, tmp.path(), &names, true, false, FAKE);
    let files_only = Mode {
        dry_run: false,
        restart: false,
    };
    assert_eq!(
        (r.action, r.exit, deploys()),
        (Action::Ensure, 0, vec![files_only])
    );
}

/// The real deploy over the test seed and history, with a stopped poller.
fn real_deploy(dir: &Path, names: &SessionNames, mode: Mode) -> anyhow::Result<Vec<Step>> {
    use super::super::launch_deploy::{Ctx, run, tests};
    let host = tests::FakeHost::default();
    let ctx = Ctx {
        seed: tests::seed(),
        shipped_before: &tests::history,
        host: &host,
        version: "9.9.9",
    };
    run(dir, names, mode, &ctx)
}

/// #11368 AC 1 through `up`: a write that fails mid-deploy leaves the poller
/// script whole, launches nothing and exits 1.
#[cfg(unix)]
#[test]
fn an_interrupted_deploy_through_up_launches_nothing_11368() {
    use super::super::launch_deploy::tests;
    let (tmp, dir) = fixture();
    let seeded = tests::seeded_dir();
    for (rel, bytes) in tests::snapshot(seeded.path()) {
        if let Some(d) = rel.strip_suffix('/') {
            std::fs::create_dir_all(dir.join(d)).expect("dir");
        } else {
            std::fs::write(dir.join(&rel), bytes).expect("file");
        }
    }
    std::fs::write(dir.join(tests::POLL), tests::OLD_POLL).expect("old poller");
    // Inside `dir`: the preflight refuses a symlink that leaves it.
    let target = dir.join("skill-target.md");
    std::fs::write(&target, tests::OLD_SKILL).expect("target");
    let last = dir.join(tests::last_dest());
    std::fs::remove_file(&last).expect("rm");
    std::os::unix::fs::symlink(&target, &last).expect("symlink");
    set(PaneState::Live(dir.clone()), PaneClaude::Absent, false);
    let ops = Ops {
        deploy: real_deploy,
        ..FAKE
    };
    let r = up(
        &dir,
        tmp.path(),
        &SessionNames::default_names(),
        false,
        false,
        ops,
    );
    assert_eq!((r.action, r.exit), (Action::Launch, 1), "{:?}", r.steps);
    assert!(runs().is_empty(), "launched after a failed write");
    assert_eq!(
        std::fs::read(dir.join(tests::POLL)).expect("poller"),
        tests::shipped(tests::POLL)
    );
    assert_eq!(std::fs::read(&target).expect("target"), tests::OLD_SKILL);
}
