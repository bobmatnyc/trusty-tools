//! #9171 A3b-2: the prototype's cycle-level quiet-session and `exited`
//! tests, ported by name (plan cases 11-14).
//!
//! Source: `tests/test_fleet_poll.py` at Architect commit 988152c. The
//! prototype wires `cycle()` to a fake fleet by monkeypatching `list_panes`,
//! `capture`, `host_alerts` and `self_ctx_alerts`; the ports run one
//! [`Runner::run_once`] over a scripted tmux, a quiet host and a tempdir
//! root, and read the kinds written to `alerts.jsonl`. The assertions are
//! the prototype's; its `ctx=50` comment names the stale 45% bar, and 50 is
//! over the live 25% bar too.

mod cycle_support;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use trusty_architect::FakeClock;
use trusty_architect::classify::{IDLE_AFTER, PaneState};
use trusty_architect::poller::{PollSession, PollState};

fn idle_since_zero(keys: &[&str]) -> PollState {
    let mut state = PollState::default();
    for k in keys {
        state.panes.insert(
            (*k).to_owned(),
            PaneState {
                idle_since: Some(0.0),
                errors: None,
            },
        );
    }
    state
}

// Python: test_quiet_session_suppresses_idle_but_not_ctx
#[test]
fn test_quiet_session_suppresses_idle_but_not_ctx() -> TestResult {
    let env = Env::new(&[])?;
    env.quiet("tm-parked\n")?;
    let tmux = fleet(
        &[
            ("tm-parked", "%10", "2.1.283"),
            ("tm-active", "%11", "2.1.283"),
        ],
        &screen(50),
    );
    let clock = FakeClock::new(IDLE_AFTER + 1.0);
    let mut session = PollSession::new(idle_since_zero(&["tm-parked|%10", "tm-active|%11"]));
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert_eq!(env.written("tm-parked")?, ["ctx"], "idle dropped");
    let mut active = env.written("tm-active")?;
    active.sort();
    assert_eq!(active, ["ctx", "idle"]);
    Ok(())
}

// Python: test_quiet_sessions_file_missing_means_nothing_is_quiet
#[test]
fn test_quiet_sessions_file_missing_means_nothing_is_quiet() -> TestResult {
    let env = Env::new(&[])?;
    // The prototype's default `screen(PM_TURN)` carries `ctx 30%`, over the
    // live 22% bar, so the live script writes `ctx` too: the frozen test's
    // `["idle"]` is stale. Same input; the live kinds.
    let tmux = fleet(&[("tm-active", "%11", "2.1.283")], &screen(30));
    let clock = FakeClock::new(IDLE_AFTER + 1.0);
    let mut session = PollSession::new(idle_since_zero(&["tm-active|%11"]));
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert!(!env.config.quiet_sessions.exists());
    assert_eq!(env.written("tm-active")?, ["idle", "ctx"]);
    Ok(())
}

// Python: test_exited_suppressed_for_a_fixture_shell_pane_but_not_a_real_one
#[test]
fn test_exited_suppressed_for_a_fixture_shell_pane_but_not_a_real_one() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fleet(
        &[("tm-r7", "%20", "bash"), ("tm-realproj", "%21", "bash")],
        "",
    );
    let clock = FakeClock::new(0.0);
    let mut session = PollSession::new(PollState::default());
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert!(env.written("tm-r7")?.is_empty());
    assert_eq!(env.written("tm-realproj")?, ["exited"]);
    let captured = tmux
        .calls()
        .iter()
        .filter(|c| c[0] == "capture-pane")
        .count();
    assert_eq!(captured, 0, "a shell pane is never captured");
    Ok(())
}

// Python: test_exited_suppressed_for_a_vanished_fixture_session_but_not_a_real_one
#[test]
fn test_exited_suppressed_for_a_vanished_fixture_session_but_not_a_real_one() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(0.0);
    let mut state = PollState::default();
    state
        .sessions
        .insert("tm-qa1-run".to_owned(), "%30".to_owned());
    state
        .sessions
        .insert("tm-realproj".to_owned(), "%31".to_owned());
    let mut session = PollSession::new(state);
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert!(env.written("tm-qa1-run")?.is_empty());
    assert_eq!(env.written("tm-realproj")?, ["exited"]);
    assert!(
        session.state.sessions.is_empty(),
        "the gone sessions are dropped"
    );
    Ok(())
}
