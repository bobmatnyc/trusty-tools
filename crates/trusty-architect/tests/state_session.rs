//! #9171 A3b-2: how the loop treats a `poll-state.json` it cannot use, and
//! the typed `wake_*` fields. No path here may silently advance the state or
//! drop an owed wake (the A3b-1 critic HIGH). Every file lives in a tempdir.

mod cycle_support;

use std::fs;

use cycle_support::{Env, QuietHost, TestResult, fleet};
use serde_json::{Value, json};
use trusty_architect::poller::{PollSession, PollState, StoredCount, load_state, save_state};
use trusty_architect::{Clock, FakeClock};

const NOW: f64 = 1_000.0;

/// A state that needs nothing this cycle: the band is already `open`.
fn quiet_state() -> PollState {
    PollState {
        band: Some("open".to_owned()),
        events_offset: Some(0),
        ..PollState::default()
    }
}

/// An unreadable state file (here: not UTF-8) is never overwritten: the loop
/// runs on an empty in-memory state and skips every save while the error
/// persists. Red against a loop that saves over it.
#[test]
fn load_state_io_skips_save() -> TestResult {
    let env = Env::new(&[])?;
    let bytes = [0xff_u8, 0xfe, b'{', b'}'];
    fs::write(env.state_path(), bytes)?;
    let (mut session, log) = PollSession::load(&env.config, false, NOW);
    assert!(session.save_blocked());
    assert!(log.iter().any(|l| l.contains("state ERROR")), "{log:?}");
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(NOW);
    let runner = env.runner(&tmux, &clock, &QuietHost);
    runner.run_once(&mut session)?;
    runner.run_once(&mut session)?;
    assert_eq!(fs::read(env.state_path())?, bytes, "never overwritten");
    assert!(session.save_blocked());
    Ok(())
}

/// A corrupt state file is copied to `poll-state.json.corrupt.<unix>`
/// before the loop resets and saves over it.
#[test]
fn load_state_corrupt_keeps_copy() -> TestResult {
    let env = Env::new(&[])?;
    fs::write(env.state_path(), "{not json")?;
    let (mut session, log) = PollSession::load(&env.config, false, NOW);
    let copy = env.config.inbox.join("poll-state.json.corrupt.1000");
    assert_eq!(fs::read_to_string(&copy)?, "{not json");
    assert!(log.iter().any(|l| l.contains("state CORRUPT")), "{log:?}");
    assert!(!session.save_blocked());
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(NOW);
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    let (_, err) = load_state(&env.state_path());
    assert!(err.is_none(), "the reset state was saved: {err:?}");
    Ok(())
}

/// When the corrupt copy cannot be made, the original is the only record:
/// the loop must not save over it.
#[test]
fn corrupt_state_whose_copy_fails_is_never_overwritten() -> TestResult {
    let env = Env::new(&[])?;
    fs::write(env.state_path(), "{not json")?;
    fs::write(
        env.config.inbox.join("poll-state.json.corrupt.1000"),
        "older",
    )?;
    let (mut session, log) = PollSession::load(&env.config, false, NOW);
    assert!(session.save_blocked(), "{log:?}");
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(NOW);
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert_eq!(fs::read_to_string(env.state_path())?, "{not json");
    Ok(())
}

/// A dry run reads a corrupt file but copies nothing.
#[test]
fn dry_run_load_writes_no_corrupt_copy() -> TestResult {
    let env = Env::new(&[])?;
    fs::write(env.state_path(), "{not json")?;
    let (session, _) = PollSession::load(&env.config, true, NOW);
    assert_eq!(session.state, PollState::default());
    let copies = fs::read_dir(&env.config.inbox)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains("corrupt"))
        .count();
    assert_eq!(copies, 0);
    Ok(())
}

/// Once a blocked file reads again, the loop adopts it and keeps the wakes
/// it owed in memory meanwhile: stored 1 + owed 2 = 3. Red against a loop
/// that stays blocked, or one that drops the in-memory count.
#[test]
fn blocked_state_recovers_and_keeps_owed_wake() -> TestResult {
    let env = Env::new(&[])?;
    fs::write(env.state_path(), [0xff_u8])?;
    let (mut session, _) = PollSession::load(&env.config, false, NOW);
    session.state.wake_pending = Some(StoredCount::Count(2));
    let mut stored = quiet_state();
    stored.wake_pending = Some(StoredCount::Count(1));
    stored
        .other
        .insert("usage".to_owned(), json!({"kept": true}));
    save_state(&env.state_path(), &stored)?;

    let tmux = fleet(&[], "");
    let clock = FakeClock::new(NOW);
    let summary = env
        .runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert!(!session.save_blocked());
    assert_eq!(session.state.wake_pending, Some(StoredCount::Count(3)));
    assert_eq!(
        summary.wake.as_deref(),
        Some("skipped: no tm-architect pane")
    );
    let (saved, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    assert_eq!(saved.wake_pending, Some(StoredCount::Count(3)));
    assert_eq!(saved.other.get("usage"), Some(&json!({"kept": true})));
    Ok(())
}

/// `wake_pending`, `wake_logged` and `wake_sent_at` are typed fields. A
/// stored value that is not a count loads (the file is not corrupt), stays
/// visible as unreadable, and `wake_pending` still reads as a wake owed.
#[test]
fn wake_fields_are_typed_and_unreadable_counts_survive_load() -> TestResult {
    let env = Env::new(&[])?;
    let text = r#"{"band": "open", "events_offset": 0, "wake_pending": "3", "wake_logged": 2, "wake_sent_at": 99.5, "rc": {"x": 1}}"#;
    fs::write(env.state_path(), text)?;
    let (state, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    assert_eq!(
        state.wake_pending,
        Some(StoredCount::Unreadable(Value::from("3")))
    );
    assert_eq!(state.wake_logged, Some(StoredCount::Count(2)));
    assert_eq!(state.wake_sent_at, Some(99.5));
    assert!(
        !state.other.contains_key("wake_pending"),
        "typed, not in other"
    );
    assert_eq!(state.other.get("rc"), Some(&json!({"x": 1})));

    let mut session = PollSession::new(state);
    let tmux = fleet(&[], "");
    let clock = FakeClock::new(NOW);
    let summary = env
        .runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert_eq!(
        summary.wake.as_deref(),
        Some("skipped: no tm-architect pane")
    );
    assert_eq!(session.state.wake_pending, Some(StoredCount::Count(1)));
    assert_eq!(clock.now_unix(), NOW);
    Ok(())
}

/// The typed fields keep the prototype's JSON: plain numbers, and absent
/// when unset.
#[test]
fn wake_fields_serialize_like_the_prototype() -> TestResult {
    let state = PollState {
        wake_pending: Some(StoredCount::Count(4)),
        wake_sent_at: Some(1.5),
        ..PollState::default()
    };
    let v = serde_json::to_value(&state)?;
    assert_eq!(v, json!({"wake_pending": 4, "wake_sent_at": 1.5}));
    Ok(())
}
