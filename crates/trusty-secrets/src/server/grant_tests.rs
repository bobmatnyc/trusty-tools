//! Tests for the exec-grant registry (#9070).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::server::grant_fakes::{FakeClock, FakeProcs};

const T0: u64 = 1_000_000;
const CHILD: u32 = 20;

struct Fixture {
    procs: Arc<FakeProcs>,
    clock: Arc<FakeClock>,
    registry: GrantRegistry,
}

fn fixture() -> Fixture {
    let procs = Arc::new(FakeProcs::sample_tree());
    let clock = Arc::new(FakeClock::at(T0));
    let registry = GrantRegistry::new(procs.clone(), clock.clone(), DEFAULT_MAX_TTL);
    Fixture {
        procs,
        clock,
        registry,
    }
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).expect("valid key")
}

fn keys(names: &[&str]) -> BTreeSet<SecretKey> {
    names.iter().map(|n| key(n)).collect()
}

fn request(ttl_secs: u64) -> GrantRequest {
    GrantRequest::new(
        keys(&["API_TOKEN", "DB_URL"]),
        CHILD,
        Duration::from_secs(ttl_secs),
    )
}

fn mint(f: &Fixture, ttl_secs: u64) -> GrantToken {
    f.registry.mint(request(ttl_secs)).expect("mint").token
}

#[test]
fn grant_token_is_256_bit_and_stored_only_as_a_hash() {
    let f = fixture();
    let a = mint(&f, 60);
    let b = mint(&f, 60);
    assert_eq!(a.expose().len(), 64);
    assert!(a.expose().bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a.expose(), b.expose());
    assert_eq!(format!("{a:?}"), "GrantToken(<redacted>)");
    // A `Grant` has no token field; its one token-derived field is the SHA-256.
    let grants = f.registry.grants.lock().expect("lock");
    let stored: Vec<TokenHash> = grants.iter().map(|g| g.token_hash).collect();
    assert_eq!(stored, vec![hash_token(&a), hash_token(&b)]);
}

#[test]
fn constant_time_eq_matches_only_identical_hashes() {
    let a = [7u8; 32];
    let mut b = a;
    assert!(constant_time_eq(&a, &b));
    b[31] ^= 1;
    assert!(!constant_time_eq(&a, &b));
}

#[test]
fn wrong_expired_and_unknown_tokens_return_the_same_error() {
    let f = fixture();
    let good = mint(&f, 10);
    let wanted = [key("API_TOKEN")];
    let mut wrong = good.expose().to_string();
    wrong.replace_range(0..1, if wrong.starts_with('0') { "1" } else { "0" });
    let wrong = f
        .registry
        .authorize(&GrantToken::from_wire(wrong), &wanted, CHILD);
    let unknown = f
        .registry
        .authorize(&GrantToken::from_wire("not-a-token"), &wanted, CHILD);
    f.clock.set(T0 + 10);
    let expired = f.registry.authorize(&good, &wanted, CHILD);
    assert_eq!(wrong, Err(GrantError::Refused));
    assert_eq!(unknown, Err(GrantError::Refused));
    assert_eq!(expired, Err(GrantError::Refused));
}

#[test]
fn grant_use_after_expiry_is_refused() {
    let f = fixture();
    let token = mint(&f, 10);
    let wanted = [key("API_TOKEN")];
    f.clock.set(T0 + 9);
    assert_eq!(f.registry.authorize(&token, &wanted, CHILD), Ok(()));
    f.clock.set(T0 + 10);
    assert_eq!(
        f.registry.authorize(&token, &wanted, CHILD),
        Err(GrantError::Refused)
    );
}

#[test]
fn grant_ttl_is_capped_at_max_ttl() {
    let f = fixture();
    let minted = f.registry.mint(request(10 * 60 * 60)).expect("mint");
    assert_eq!(minted.ttl, DEFAULT_MAX_TTL);
    assert_eq!(minted.expires_at, Duration::from_secs(T0) + DEFAULT_MAX_TTL);
    f.clock.set(T0 + DEFAULT_MAX_TTL.as_secs());
    assert_eq!(
        f.registry.authorize(&minted.token, &[key("DB_URL")], CHILD),
        Err(GrantError::Refused)
    );
}

#[test]
fn one_shot_grant_refuses_second_use() {
    let f = fixture();
    let token = f.registry.mint(request(60).one_shot()).expect("mint").token;
    let wanted = [key("API_TOKEN"), key("DB_URL")];
    assert_eq!(f.registry.authorize(&token, &wanted, CHILD), Ok(()));
    assert_eq!(
        f.registry.authorize(&token, &wanted, CHILD),
        Err(GrantError::Refused)
    );
}

#[test]
fn key_outside_grant_is_refused() {
    let f = fixture();
    let token = mint(&f, 60);
    assert_eq!(
        f.registry
            .authorize(&token, &[key("API_TOKEN"), key("OTHER")], CHILD),
        Err(GrantError::Refused)
    );
}

#[test]
fn grant_allows_child_and_grandchild_and_refuses_sibling() {
    let f = fixture();
    let token = mint(&f, 60);
    let wanted = [key("API_TOKEN")];
    assert_eq!(f.registry.authorize(&token, &wanted, 20), Ok(()));
    assert_eq!(f.registry.authorize(&token, &wanted, 30), Ok(()));
    for outsider in [21, 10, 50] {
        assert_eq!(
            f.registry.authorize(&token, &wanted, outsider),
            Err(GrantError::Refused),
            "pid {outsider}"
        );
    }
}

#[test]
fn pid_reuse_with_new_start_time_is_refused() {
    let f = fixture();
    let token = mint(&f, 60);
    // The child exits and an unrelated process takes pid 20, with a child of its own.
    f.procs.add(CHILD, 50, 999).add(31, CHILD, 1000);
    let wanted = [key("API_TOKEN")];
    assert_eq!(
        f.registry.authorize(&token, &wanted, CHILD),
        Err(GrantError::Refused)
    );
    assert_eq!(
        f.registry.authorize(&token, &wanted, 31),
        Err(GrantError::Refused)
    );
}

#[test]
fn revoke_removes_the_grant() {
    let f = fixture();
    let token = mint(&f, 60);
    assert_eq!(f.registry.revoke(&token), Ok(true));
    assert_eq!(
        f.registry.authorize(&token, &[key("API_TOKEN")], CHILD),
        Err(GrantError::Refused)
    );
    assert_eq!(f.registry.revoke(&token), Ok(false));
}

#[test]
fn has_unexpired_tracks_expiry_and_revoke() {
    let f = fixture();
    assert_eq!(f.registry.has_unexpired(), Ok(false));
    let short = mint(&f, 10);
    let long = mint(&f, 20);
    assert_eq!(f.registry.has_unexpired(), Ok(true));
    f.clock.set(T0 + 10);
    assert_eq!(f.registry.has_unexpired(), Ok(true));
    assert_eq!(
        f.registry.revoke(&short),
        Ok(false),
        "expired grant was dropped"
    );
    assert_eq!(f.registry.revoke(&long), Ok(true));
    assert_eq!(f.registry.has_unexpired(), Ok(false));
}

#[test]
fn registry_poisoned_lock_denies() {
    let f = fixture();
    let token = mint(&f, 60);
    // A panic inside the locked ancestry walk poisons the registry lock.
    f.procs.panic_on_start(CHILD);
    let panicked = std::thread::scope(|s| {
        s.spawn(|| f.registry.authorize(&token, &[key("API_TOKEN")], CHILD))
            .join()
    });
    assert!(panicked.is_err(), "the injected panic must fire");
    let poisoned = Err(GrantError::RegistryPoisoned);
    assert_eq!(
        f.registry.authorize(&token, &[key("API_TOKEN")], 30),
        poisoned
    );
    assert_eq!(f.registry.revoke(&token).map(|_| ()), poisoned);
    assert_eq!(f.registry.has_unexpired().map(|_| ()), poisoned);
    assert_eq!(f.registry.mint(request(60)).map(|_| ()), poisoned);
}

#[test]
fn unreadable_process_table_denies() {
    let f = fixture();
    let token = mint(&f, 60);
    f.procs.set_unreadable();
    assert_eq!(
        f.registry.authorize(&token, &[key("API_TOKEN")], 30),
        Err(GrantError::Process(ProcessError::Unreadable { pid: 30 }))
    );
    assert_eq!(
        f.registry.mint(request(60)).map(|_| ()),
        Err(GrantError::Process(ProcessError::Unreadable { pid: CHILD }))
    );
}

#[test]
fn unreadable_start_time_denies() {
    let f = fixture();
    let token = mint(&f, 60);
    f.procs.set_start_unreadable(CHILD);
    let unreadable = Err(GrantError::Process(ProcessError::StartTimeUnreadable {
        pid: CHILD,
    }));
    assert_eq!(
        f.registry.authorize(&token, &[key("API_TOKEN")], 30),
        unreadable
    );
    assert_eq!(f.registry.mint(request(60)).map(|_| ()), unreadable);
}

#[test]
fn clock_error_denies() {
    let f = fixture();
    let token = mint(&f, 60);
    f.clock.fail();
    let clock = Err(GrantError::Clock);
    assert_eq!(
        f.registry.authorize(&token, &[key("API_TOKEN")], CHILD),
        clock
    );
    assert_eq!(f.registry.mint(request(60)).map(|_| ()), clock);
    assert_eq!(f.registry.has_unexpired().map(|_| ()), clock);
    // A clock moved back past the mint time denies too.
    f.clock.set(T0 - 1);
    assert_eq!(
        f.registry.authorize(&token, &[key("API_TOKEN")], CHILD),
        clock
    );
}

#[test]
fn mint_rejects_invalid_requests() {
    let f = fixture();
    let invalid = |r: GrantRequest| f.registry.mint(r).map(|_| ());
    assert_eq!(
        invalid(GrantRequest::new(
            BTreeSet::new(),
            CHILD,
            Duration::from_secs(60)
        )),
        Err(GrantError::InvalidRequest("no keys"))
    );
    assert_eq!(
        invalid(GrantRequest::new(keys(&["A"]), 1, Duration::from_secs(60))),
        Err(GrantError::InvalidRequest("child pid"))
    );
    assert_eq!(
        invalid(GrantRequest::new(keys(&["A"]), CHILD, Duration::ZERO)),
        Err(GrantError::InvalidRequest("zero ttl"))
    );
    let token = mint(&f, 60);
    assert_eq!(
        f.registry.authorize(&token, &[], CHILD),
        Err(GrantError::InvalidRequest("no keys"))
    );
}

#[test]
fn live_grant_count_is_capped() {
    let f = fixture();
    for _ in 0..MAX_LIVE_GRANTS {
        mint(&f, 10);
    }
    assert_eq!(
        f.registry.mint(request(10)).map(|_| ()),
        Err(GrantError::CapacityReached)
    );
    // Expired grants free their slots.
    f.clock.set(T0 + 10);
    assert!(f.registry.mint(request(10)).is_ok());
}
