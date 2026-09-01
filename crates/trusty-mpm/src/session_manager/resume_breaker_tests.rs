//! Tests for the #6568 auto-resume circuit breaker.
//!
//! Why: the live defect is a two-process, 60-second cycle that ran 2,128 times
//! in 48 hours. Reproducing it means driving the SAME two calls the daemon and
//! the supervisor make — `mark_runtime_exited_stopped` and `resume_auto` — with
//! a compressed window, and asserting the streak eventually parks the session.
//! What: the pure-policy cases against [`super::resume_breaker::evaluate`], the
//! sidecar's failure paths, and the end-to-end park/unpark cycle against a real
//! [`SessionManager`] on a [`FakeNoopTmuxDriver`].
//! Test: this is the test module.

use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeDelta, Utc};

use super::record::{ManagedSessionId, ManagedSessionState, StopCause};
use super::resume_breaker::{
    BreakerVerdict, DEFAULT_MAX_CONSECUTIVE, ENV_FLAP_WINDOW_SECS, ENV_MAX_CONSECUTIVE, FlapState,
    ResumeBreakerConfig, ResumeBreakerStore, evaluate,
};
use super::{FakeNoopTmuxDriver, SessionManager};

// -------------------------------------------------------------------------
// Config
// -------------------------------------------------------------------------

#[test]
fn config_defaults() {
    let cfg = ResumeBreakerConfig::default();
    assert_eq!(cfg.flap_window, Duration::from_secs(120));
    assert_eq!(cfg.max_consecutive, DEFAULT_MAX_CONSECUTIVE);
}

#[test]
fn config_env_parsing() {
    let cfg = ResumeBreakerConfig::from_env_with(|k| match k {
        ENV_FLAP_WINDOW_SECS => Some(" 45 ".to_string()),
        ENV_MAX_CONSECUTIVE => Some("3".to_string()),
        _ => None,
    });
    assert_eq!(cfg.flap_window, Duration::from_secs(45));
    assert_eq!(cfg.max_consecutive, 3);

    // Garbage falls back rather than silently disabling the breaker.
    let cfg = ResumeBreakerConfig::from_env_with(|_| Some("nope".to_string()));
    assert_eq!(cfg, ResumeBreakerConfig::default());

    // A zero threshold would park a session on its zeroth flap — normalised.
    let cfg =
        ResumeBreakerConfig::from_env_with(|k| (k == ENV_MAX_CONSECUTIVE).then(|| "0".to_string()));
    assert_eq!(cfg.max_consecutive, 1);
}

// -------------------------------------------------------------------------
// Pure policy
// -------------------------------------------------------------------------

fn cfg(window_secs: u64, k: u32) -> ResumeBreakerConfig {
    ResumeBreakerConfig {
        flap_window: Duration::from_secs(window_secs),
        max_consecutive: k,
    }
}

#[test]
fn evaluate_resets_without_a_prior_auto_resume() {
    // A session nobody auto-resumed cannot be evidence that auto-resume is
    // failing to fix anything — the very first crash must stay resumable.
    let v = evaluate(&cfg(120, 3), &FlapState::default(), Utc::now());
    assert_eq!(v, BreakerVerdict::Reset);
}

#[test]
fn evaluate_resets_for_a_slow_death() {
    let now = Utc::now();
    let state = FlapState {
        last_auto_resume_at: Some(now - TimeDelta::seconds(500)),
        consecutive: 2,
    };
    assert_eq!(evaluate(&cfg(120, 3), &state, now), BreakerVerdict::Reset);
}

#[test]
fn evaluate_counts_a_fast_death() {
    let now = Utc::now();
    let state = FlapState {
        last_auto_resume_at: Some(now - TimeDelta::seconds(60)),
        consecutive: 1,
    };
    assert_eq!(
        evaluate(&cfg(120, 5), &state, now),
        BreakerVerdict::Counting { consecutive: 2 }
    );
}

#[test]
fn evaluate_parks_at_the_threshold() {
    let now = Utc::now();
    let state = FlapState {
        last_auto_resume_at: Some(now - TimeDelta::seconds(60)),
        consecutive: 2,
    };
    assert_eq!(
        evaluate(&cfg(120, 3), &state, now),
        BreakerVerdict::Park { consecutive: 3 }
    );
}

#[test]
fn evaluate_resets_on_a_backwards_clock() {
    // A death "before" the resume that preceded it is a clock step, not the
    // fastest possible death. Reading it as age zero would park a healthy
    // session the moment NTP corrected the host.
    let now = Utc::now();
    let state = FlapState {
        last_auto_resume_at: Some(now + TimeDelta::seconds(600)),
        consecutive: 4,
    };
    assert_eq!(evaluate(&cfg(120, 5), &state, now), BreakerVerdict::Reset);
}

#[test]
fn a_zero_window_disables_the_breaker() {
    // The documented escape hatch: no death is ever inside a zero-length
    // window, so an operator can restore the pre-#6568 behavior exactly.
    let now = Utc::now();
    let state = FlapState {
        last_auto_resume_at: Some(now),
        consecutive: 99,
    };
    assert_eq!(evaluate(&cfg(0, 1), &state, now), BreakerVerdict::Reset);
}

#[test]
fn verdict_reports_its_streak_length() {
    assert_eq!(BreakerVerdict::Reset.consecutive(), 0);
    assert_eq!(BreakerVerdict::Counting { consecutive: 2 }.consecutive(), 2);
    assert_eq!(BreakerVerdict::Park { consecutive: 7 }.consecutive(), 7);
}

// -------------------------------------------------------------------------
// Sidecar store — including the failure paths (Fail-Open Check)
// -------------------------------------------------------------------------

#[test]
fn store_round_trips_state() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let id = ManagedSessionId::new();
    let now = Utc::now();
    {
        let mut store = ResumeBreakerStore::load(tmp.path());
        store.note_auto_resume(&id, now);
        assert_eq!(
            store.record_death(&id, &cfg(120, 3), now),
            BreakerVerdict::Counting { consecutive: 1 }
        );
    }
    // A fresh load in a DIFFERENT process would see the same counter — this is
    // why the sidecar exists at all (the reaper and the poller are separate
    // processes).
    let reloaded = ResumeBreakerStore::load(tmp.path());
    assert_eq!(reloaded.state_of(&id).consecutive, 1);
}

#[test]
fn store_survives_a_missing_file() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let store = ResumeBreakerStore::load(tmp.path());
    assert_eq!(
        store.state_of(&ManagedSessionId::new()),
        FlapState::default()
    );
}

#[test]
fn store_survives_a_corrupt_file() {
    // Fail-open: a corrupt sidecar starts empty rather than erroring. Losing
    // the counter can only DELAY a park; it can never cause one, because the
    // park itself lives on the record.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join(ResumeBreakerStore::FILE_NAME), "{ not json")
        .expect("write corrupt sidecar");
    let store = ResumeBreakerStore::load(tmp.path());
    assert_eq!(
        store.state_of(&ManagedSessionId::new()),
        FlapState::default()
    );
}

#[test]
fn a_reset_forgets_the_stamp_as_well_as_the_count() {
    // After a slow death the previous auto-resume is no longer the thing this
    // session died after, so the NEXT death must not be attributed to it.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let id = ManagedSessionId::new();
    let now = Utc::now();
    let mut store = ResumeBreakerStore::load(tmp.path());
    store.note_auto_resume(&id, now - TimeDelta::seconds(500));
    assert_eq!(
        store.record_death(&id, &cfg(120, 2), now),
        BreakerVerdict::Reset
    );
    assert_eq!(store.state_of(&id), FlapState::default());
    // A second death immediately afterwards is still not a flap.
    assert_eq!(
        store.record_death(&id, &cfg(120, 2), now),
        BreakerVerdict::Reset
    );
}

#[test]
fn a_fast_death_after_an_auto_resume_counts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let id = ManagedSessionId::new();
    let now = Utc::now();
    let mut store = ResumeBreakerStore::load(tmp.path());
    store.note_auto_resume(&id, now);
    assert_eq!(
        store.record_death(&id, &cfg(120, 9), now + TimeDelta::seconds(30)),
        BreakerVerdict::Counting { consecutive: 1 }
    );
}

// -------------------------------------------------------------------------
// End to end, through the real SessionManager
// -------------------------------------------------------------------------

/// Build an isolated manager whose breaker parks after `k` fast deaths, and
/// seed one `Active` session on a fake tmux driver.
async fn seeded(dir: &tempfile::TempDir, k: u32) -> (Arc<SessionManager>, ManagedSessionId) {
    let mut mgr = SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
        .await
        .expect("session manager");
    // A one-hour window makes every death in this test a "fast" one without
    // any sleeping; `k` is the only knob under test.
    mgr.set_resume_breaker_config(ResumeBreakerConfig {
        flap_window: Duration::from_secs(3600),
        max_consecutive: k,
    });
    let mgr = Arc::new(mgr);
    let record = mgr
        .create(
            "flap-test".into(),
            Some(std::path::PathBuf::from("/tmp")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(
        &id,
        std::path::PathBuf::from("/tmp"),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");
    (mgr, id)
}

/// Drive one full cycle of the live defect: the reaper marks the session
/// `Stopped`, the supervisor auto-resumes it, and the pane is unchanged so it
/// dies again. Returns the record as the reaper left it.
async fn one_cycle(mgr: &Arc<SessionManager>, id: &ManagedSessionId) -> super::SessionRecord {
    let stopped = mgr
        .mark_runtime_exited_stopped(id)
        .await
        .expect("mark stopped");
    if stopped.is_auto_resumable() {
        mgr.resume_auto(id).await.expect("auto-resume");
    }
    stopped
}

/// The headline regression. RED before the fix: every cycle wrote
/// `StopCause::Unexpected`, `is_auto_resumable` stayed true forever, and this
/// loop would never terminate the thrash — which is exactly the 2,128
/// resumes/48h the audit measured.
#[tokio::test]
async fn the_supervisor_parks_a_flapping_session_after_k_cycles() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (mgr, id) = seeded(&tmp, 3).await;

    // The first death follows no auto-resume, so it resets and resumes.
    let first = one_cycle(&mgr, &id).await;
    assert_eq!(first.stop_cause, Some(StopCause::Unexpected));
    assert!(first.is_auto_resumable());

    // Two more fast deaths build the streak without parking.
    for cycle in 1..=2 {
        let r = one_cycle(&mgr, &id).await;
        assert_eq!(
            r.stop_cause,
            Some(StopCause::Unexpected),
            "cycle {cycle} must still be resumable"
        );
        assert!(r.is_auto_resumable(), "cycle {cycle}");
    }

    // The third fast death reaches the threshold.
    let parked = one_cycle(&mgr, &id).await;
    assert_eq!(
        parked.stop_cause,
        Some(StopCause::ResumeFlapping),
        "the breaker must trip at K consecutive fast deaths (#6568)"
    );
    assert!(
        !parked.is_auto_resumable(),
        "a parked session must be invisible to every automatic resume path"
    );

    // And it STAYS parked: the store's own record still refuses.
    let after = mgr.get(&id).await.expect("get");
    assert_eq!(after.stop_cause, Some(StopCause::ResumeFlapping));
    assert!(!after.is_auto_resumable());
}

/// The park must be operator-recoverable, or it is a trap rather than a
/// breaker. A manual `resume` clears both the cause and the streak.
#[tokio::test]
async fn an_operator_resume_forgives_the_streak() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let (mgr, id) = seeded(&tmp, 2).await;

    one_cycle(&mgr, &id).await; // reset + resume
    one_cycle(&mgr, &id).await; // streak 1, resumed
    let parked = mgr
        .mark_runtime_exited_stopped(&id)
        .await
        .expect("mark stopped");
    assert_eq!(parked.stop_cause, Some(StopCause::ResumeFlapping));

    // The operator fixes the cause and resumes by hand.
    let revived = mgr.resume(&id).await.expect("operator resume");
    assert_eq!(revived.state, ManagedSessionState::Active);
    assert_eq!(revived.stop_cause, None);

    // The next fast death must NOT re-park immediately — the streak was
    // forgiven, so the session gets its full budget back.
    let next = mgr
        .mark_runtime_exited_stopped(&id)
        .await
        .expect("mark stopped");
    assert_eq!(
        next.stop_cause,
        Some(StopCause::Unexpected),
        "an operator resume must restore the full budget, not one death of it"
    );
    assert!(next.is_auto_resumable());
}

/// A session that runs for longer than the window between deaths is a crashing
/// session, not a flapping one, and auto-resume must keep helping it.
#[tokio::test]
async fn a_session_that_dies_slowly_is_never_parked() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut mgr = SessionManager::new(tmp.path(), Arc::new(FakeNoopTmuxDriver))
        .await
        .expect("session manager");
    // A zero window means no death is ever "fast" — the slow-death case in the
    // limit, with no sleeping.
    mgr.set_resume_breaker_config(ResumeBreakerConfig {
        flap_window: Duration::from_secs(0),
        max_consecutive: 2,
    });
    let mgr = Arc::new(mgr);
    let record = mgr
        .create(
            "slow".into(),
            Some(std::path::PathBuf::from("/tmp")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr.set_workspace(
        &id,
        std::path::PathBuf::from("/tmp"),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");

    for cycle in 1..=6 {
        let r = one_cycle(&mgr, &id).await;
        assert_eq!(
            r.stop_cause,
            Some(StopCause::Unexpected),
            "cycle {cycle}: a slow death must never park the session"
        );
        assert!(r.is_auto_resumable(), "cycle {cycle}");
    }
}

/// Build a `Stopped` record carrying `cause`, through the real create path.
async fn stopped_record_with(
    dir: &tempfile::TempDir,
    cause: Option<StopCause>,
) -> super::SessionRecord {
    let mgr = SessionManager::new(dir.path(), Arc::new(FakeNoopTmuxDriver))
        .await
        .expect("session manager");
    let mut r = mgr
        .create(
            "cause".into(),
            Some(std::path::PathBuf::from("/tmp")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    r.state = ManagedSessionState::Stopped;
    r.stop_cause = cause;
    r
}

/// `auto_resume` control-state semantics are untouched: the breaker expresses
/// itself through `stop_cause`, the same gate `Deliberate` already used, and it
/// never relabels a deliberately-stopped record as a breaker trip.
#[tokio::test]
async fn park_reason_is_set_only_for_a_flapping_record() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    for cause in [
        None,
        Some(StopCause::Unexpected),
        Some(StopCause::Deliberate),
    ] {
        let r = stopped_record_with(&tmp, cause).await;
        assert!(
            r.auto_resume_park_reason().is_none(),
            "{cause:?} is not a breaker trip"
        );
    }

    let parked = stopped_record_with(&tmp, Some(StopCause::ResumeFlapping)).await;
    let reason = parked
        .auto_resume_park_reason()
        .expect("a flapping record must name its park reason");
    assert!(reason.contains("#6568"), "reason was: {reason}");
    assert!(
        reason.contains("tm session resume"),
        "the reason must tell an operator how to clear it: {reason}"
    );
}

/// The wire shape every listing surface reads carries the reason (#6568).
#[tokio::test]
async fn summary_carries_the_resume_park_reason() {
    use crate::daemon::managed_routes::summary::record_to_summary;

    let tmp = tempfile::TempDir::new().expect("tempdir");

    let ordinary = stopped_record_with(&tmp, Some(StopCause::Unexpected)).await;
    assert!(record_to_summary(&ordinary).auto_resume_parked.is_none());

    let parked = stopped_record_with(&tmp, Some(StopCause::ResumeFlapping)).await;
    assert_eq!(
        record_to_summary(&parked).auto_resume_parked.as_deref(),
        parked.auto_resume_park_reason(),
        "the summary must not restate the reason in its own words"
    );
}
