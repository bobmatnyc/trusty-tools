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
fn record(action: Action, _: &Path, _: &Path, _: &SessionNames) -> anyhow::Result<Outcome> {
    RUNS.with(|r| r.borrow_mut().push(action));
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
};

/// A scratch home with the Architect directory `home/arch`.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("arch")).expect("arch");
    let dir = std::fs::canonicalize(tmp.path().join("arch")).expect("canonical");
    RUNS.with(|r| r.borrow_mut().clear());
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
