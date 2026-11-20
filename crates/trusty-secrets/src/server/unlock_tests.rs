//! The unlock grant's socket methods and sentinel (#10524).
//!
//! A child of `session_cache_tests`, so it shares the counting backend
//! double, the fixture and the method-body helpers. The power source is a
//! fake: no test registers with the OS or shows a prompt. Each failure-arm
//! test asserts the backend is read again, so a variant that served the
//! cached value instead goes red.
//!
//! Test: itself.

use std::time::Duration;

use super::*;
use crate::server::audit::ServedFrom;
use crate::server::power::{Hooks, PowerEvent, PowerHook, PowerSource, Subscription, WatchError};
use crate::server::unlock;
use crate::store::grant_cache::{DropReason, Status};

/// A power source a test drives: it can refuse to start, die, or fire.
#[derive(Default)]
struct FakePower {
    refuse: AtomicBool,
    dead: AtomicBool,
    hooks: Arc<Hooks>,
}

impl PowerSource for FakePower {
    fn subscribe(&self, hook: PowerHook) -> Result<Subscription, WatchError> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err(WatchError);
        }
        self.hooks.add(hook)
    }

    fn alive(&self) -> bool {
        !self.dead.load(Ordering::SeqCst)
    }
}

/// A Keychain double holding `API_KEY`, a state over it with `power`, and
/// an exec grant for this process.
fn unlock_state(fx: &Fixture, power: Arc<FakePower>) -> (Arc<Counting>, State, String) {
    let kc = Counting::new(BackendId::keychain());
    put(fx, &kc, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let mut state = cache_state(fx, std::slice::from_ref(&kc), procs_with_me());
    state.power = power;
    state.grant_tick = Duration::from_millis(5);
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    (kc, state, token)
}

/// A grant registry over `procs_with_me`, with no agent ancestor.
fn procs_grants() -> Arc<GrantRegistry> {
    Arc::new(GrantRegistry::new(
        procs_with_me(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ))
}

fn unlock_now(state: &State, caller: Caller) -> Result<Value, ErrorKind> {
    unlock::unlock(state, caller, json!({"project": "ignored"}))
}

/// The `grant_drop` reasons of the `secrets.unlock.drop` records.
fn drops(fx: &Fixture) -> Vec<DropReason> {
    records(fx)
        .iter()
        .filter(|r| r.method == AuditMethod::GrantDrop)
        .filter_map(|r| r.grant_drop)
        .collect()
}

/// Wait up to 2 s for the sentinel alone to drop the grant and audit it;
/// nothing here checks the grant, so only the sentinel can.
fn wait_for_drop(fx: &Fixture) {
    for _ in 0..400 {
        if !drops(fx).is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the sentinel never dropped the grant");
}

/// Why: #10524 AC1, AC5, AC6 — after one unlock, repeated Keychain reads
/// read the backend once; each read is still audited and marked
/// `served_from: grant`. Lock ends it: the next read goes back to the
/// backend, and the lock record names `revoked`.
/// Test: itself.
#[test]
fn unlock_then_resolve_reads_the_keychain_once() {
    let fx = fixture();
    let (kc, state, token) = unlock_state(&fx, Arc::default());
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(
        kc.gets(),
        2,
        "without a grant every read reaches the Keychain"
    );

    let answer = unlock_now(&state, caller()).unwrap();
    assert_eq!(answer["expires_in_secs"], 8 * 3600);
    for _ in 0..3 {
        assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    }
    assert_eq!(kc.gets(), 3, "the grant did not answer repeat reads");
    assert!(
        unlock::grant_keeps_serving(&state),
        "idle exit not deferred"
    );

    let locked = unlock::lock(&state, caller(), json!({})).unwrap();
    assert_eq!(locked["locked"], true);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    assert_eq!(kc.gets(), 4, "a read after lock was served from memory");
    assert!(!unlock::grant_keeps_serving(&state));

    let all = records(&fx);
    let marked = all
        .iter()
        .filter(|r| r.method == AuditMethod::Resolve && r.served_from == Some(ServedFrom::Grant))
        .count();
    assert_eq!(marked, 3, "reads under the grant were not marked");
    assert!(all.iter().any(|r| r.method == AuditMethod::Unlock
        && r.decision == AuditDecision::Allow
        && r.agent_parent == Some(false)));
    assert!(
        all.iter()
            .any(|r| r.method == AuditMethod::Lock && r.grant_drop == Some(DropReason::Revoked))
    );
    let text = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(
        !text.contains(SENTINEL_VALUE),
        "a value reached the audit log"
    );
}

/// Why: #10524 AC5 — an agent, or a caller whose ancestry cannot be read,
/// never opens the grant; the refusal is audited and reads still prompt.
/// Test: itself.
#[test]
fn unlock_by_agent_descendant_is_refused_and_opens_nothing() {
    let fx = fixture();
    let (kc, mut agent_state, _) = unlock_state(&fx, Arc::default());
    let procs = procs_with_me();
    procs.set_agent(10);
    agent_state.grants = Arc::new(GrantRegistry::new(
        procs,
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    assert_eq!(
        unlock_now(&agent_state, caller()),
        Err(ErrorKind::AgentUseRefused)
    );
    assert_eq!(
        unlock_now(&agent_state, Caller::new(Some(999_999))),
        Err(ErrorKind::AgentUseRefused),
        "an unreadable ancestry opened the grant"
    );
    assert_eq!(
        unlock_now(&agent_state, Caller::new(None)),
        Err(ErrorKind::AgentUseRefused)
    );
    assert_eq!(agent_state.cache.grant().check(), Status::Off);
    agent_state.grants = procs_grants();
    value_of(
        &agent_state,
        &grant_in(&agent_state, &fx.project(), &["API_KEY"]),
        "API_KEY",
    );
    value_of(
        &agent_state,
        &grant_in(&agent_state, &fx.project(), &["API_KEY"]),
        "API_KEY",
    );
    assert_eq!(kc.gets(), 2, "a refused unlock still cached a value");
    let denied = records(&fx)
        .iter()
        .filter(|r| r.method == AuditMethod::Unlock && r.decision == AuditDecision::Deny)
        .count();
    assert_eq!(denied, 3);
}

/// Why: #10524 AC3, AC7 — a power watcher that cannot start opens no grant;
/// the reply is an error and every read still reaches the Keychain.
/// Test: itself.
#[test]
fn unlock_watcher_that_cannot_start_opens_no_grant() {
    let fx = fixture();
    let power = Arc::new(FakePower::default());
    power.refuse.store(true, Ordering::SeqCst);
    let (kc, state, token) = unlock_state(&fx, power);
    assert_eq!(unlock_now(&state, caller()), Err(ErrorKind::Internal));
    assert_eq!(state.cache.grant().check(), Status::Off);
    value_of(&state, &token, "API_KEY");
    value_of(&state, &token, "API_KEY");
    assert_eq!(kc.gets(), 2, "a grant opened with no watcher");
}

/// Why: #10524 AC3, AC6 — a system-sleep event drops the grant before the
/// machine sleeps, audited as `sleep`; the next read prompts again.
/// Test: itself.
#[test]
fn sleep_event_drops_the_grant_and_audits_it() {
    let fx = fixture();
    let power = Arc::new(FakePower::default());
    let (kc, state, token) = unlock_state(&fx, Arc::clone(&power));
    unlock_now(&state, caller()).unwrap();
    value_of(&state, &token, "API_KEY");
    assert!(power.hooks.fire(PowerEvent::Sleep));
    assert_eq!(state.cache.grant().check(), Status::Off);
    assert_eq!(state.cache.grant().count(), 0, "a value outlived the sleep");
    value_of(&state, &token, "API_KEY");
    assert_eq!(kc.gets(), 2, "a read after sleep was served from memory");
    assert_eq!(drops(&fx), [DropReason::Sleep]);
}

/// Why: #10524 AC3, AC7 — a power watcher that dies after unlock drops the
/// grant (audited as `fault`); reads go back to the Keychain.
/// Test: itself.
#[test]
fn dead_power_source_drops_the_grant() {
    let fx = fixture();
    let power = Arc::new(FakePower::default());
    let (kc, state, token) = unlock_state(&fx, Arc::clone(&power));
    unlock_now(&state, caller()).unwrap();
    value_of(&state, &token, "API_KEY");
    power.dead.store(true, Ordering::SeqCst);
    wait_for_drop(&fx);
    value_of(&state, &token, "API_KEY");
    assert_eq!(kc.gets(), 2, "a read with a dead watcher was served");
    assert_eq!(drops(&fx), [DropReason::Fault]);
}

/// Why: #10524 AC3, AC6 — the sentinel ends an expired grant on its own,
/// zeroes the values and audits `expired`; no silent renewal.
/// Test: itself.
#[test]
fn expired_grant_is_dropped_and_audited_by_the_sentinel() {
    let fx = fixture();
    let (kc, mut state, token) = unlock_state(&fx, Arc::default());
    state.settings.unlock_ttl = Duration::from_millis(50);
    unlock_now(&state, caller()).unwrap();
    value_of(&state, &token, "API_KEY");
    wait_for_drop(&fx);
    assert_eq!(state.cache.grant().count(), 0);
    value_of(&state, &token, "API_KEY");
    assert_eq!(kc.gets(), 2, "an expired grant answered");
    assert_eq!(drops(&fx), [DropReason::Expired]);
}

/// Why: #10524 AC4 — a shutdown ends the grant as `logout`, and a new
/// server process (a new `State`) never starts with one.
/// Test: itself.
#[test]
fn shutdown_logs_logout_and_a_restart_restores_no_grant() {
    let fx = fixture();
    let (kc, state, token) = unlock_state(&fx, Arc::default());
    unlock_now(&state, caller()).unwrap();
    unlock::end_session(&state);
    assert_eq!(drops(&fx), [DropReason::Logout]);

    let (_, restarted, _) = unlock_state(&fx, Arc::default());
    assert_eq!(restarted.cache.grant().check(), Status::Off);
    assert!(!unlock::grant_keeps_serving(&restarted));
    value_of(&state, &token, "API_KEY");
    value_of(&state, &token, "API_KEY");
    assert_eq!(kc.gets(), 2);
}

/// Why: #10524 AC5 — the agents gate runs before the grant's cache: a key
/// denied to agents after it was cached is still refused to an agent.
/// Test: itself.
#[test]
fn grant_cache_still_refuses_denied_key_for_agent() {
    let fx = fixture();
    let kc = Counting::new(BackendId::keychain());
    put(&fx, &kc, PROJECT_VAULT, "API_KEY", SENTINEL_VALUE);
    let procs = procs_with_me();
    let mut state = cache_state(&fx, std::slice::from_ref(&kc), Arc::clone(&procs));
    state.power = Arc::new(FakePower::default());
    unlock_now(&state, caller()).unwrap();
    let token = grant_in(&state, &fx.project(), &["API_KEY"]);
    assert_eq!(value_of(&state, &token, "API_KEY"), SENTINEL_VALUE);
    procs.set_agent(10);
    flag(&fx, &kc, "API_KEY", false);
    assert_eq!(
        resolve_in(&state, &token, "API_KEY"),
        Err(ErrorKind::AgentKeyDenied)
    );
    assert_eq!(kc.gets(), 1);
}
