//! Tests for the unlock grant's cache (#10524).
//!
//! Each failure arm has its own test, and each asserts that no value is
//! served, so a variant that let the read through goes red.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use super::*;
use crate::api::SecretsError;
use crate::store::SecretCache;

const TTL: Duration = Duration::from_secs(8 * 3600);

/// Two clocks a test moves by whole seconds, independently.
struct Clocks {
    mono: Arc<AtomicI64>,
    wall: Arc<AtomicI64>,
}

impl Clocks {
    fn new() -> Self {
        Self {
            mono: Arc::new(AtomicI64::new(0)),
            wall: Arc::new(AtomicI64::new(0)),
        }
    }

    fn pair(&self) -> (CacheClock, WallClock) {
        let (at, wall) = (Instant::now(), SystemTime::now());
        let (mono, walls) = (Arc::clone(&self.mono), Arc::clone(&self.wall));
        (
            Arc::new(move || at + secs(mono.load(Ordering::SeqCst))),
            Arc::new(move || {
                let offset = walls.load(Ordering::SeqCst);
                if offset < 0 {
                    wall - secs(offset)
                } else {
                    wall + secs(offset)
                }
            }),
        )
    }

    /// Advance both clocks to `secs` after the start.
    fn both(&self, secs: i64) {
        self.mono.store(secs, Ordering::SeqCst);
        self.wall.store(secs, Ordering::SeqCst);
    }

    fn cache(&self) -> GrantCache {
        let (now, wall) = self.pair();
        GrantCache::new(now, wall)
    }
}

fn secs(n: i64) -> Duration {
    Duration::from_secs(n.unsigned_abs())
}

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn alive() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

/// Fill `name` from the Keychain under the live grant; `true` when kept.
fn fill(cache: &GrantCache, name: &str, value: &str) -> bool {
    let kc = BackendId::keychain();
    match cache.lookup(&kc, None, &vault(), &key(name)) {
        Lookup::Miss(epoch) => cache.store(
            &kc,
            None,
            &vault(),
            &key(name),
            epoch,
            &SecretValue::new(value),
        ),
        _ => false,
    }
}

/// Whether the grant serves `name` from the Keychain.
fn served(cache: &GrantCache, name: &str) -> Option<String> {
    match cache.lookup(&BackendId::keychain(), None, &vault(), &key(name)) {
        Lookup::Hit(value) => Some(value.expose().to_owned()),
        _ => None,
    }
}

/// Why: #10524 AC1 — under a live grant a Keychain value is read once and
/// then served; with no grant, or from a backend the grant does not cache,
/// nothing is held.
/// Test: itself.
#[test]
fn grant_cache_serves_keychain_values_only_while_live() {
    let clocks = Clocks::new();
    let cache = clocks.cache();
    assert!(matches!(
        cache.lookup(&BackendId::keychain(), None, &vault(), &key("K")),
        Lookup::Off
    ));
    cache.open(TTL, alive());
    assert!(fill(&cache, "K", "v"));
    assert_eq!(served(&cache, "K").as_deref(), Some("v"));
    let file = BackendId::from_static("file");
    assert!(matches!(
        cache.lookup(&file, None, &vault(), &key("K")),
        Lookup::Off
    ));
    assert!(cache.close(DropReason::Revoked, None));
    assert_eq!(served(&cache, "K"), None);
    assert_eq!(cache.count(), 0, "lock left a value behind");
    assert!(!cache.close(DropReason::Revoked, None), "closed twice");
    assert_eq!(cache.check(), Status::Off, "a closed grant was reported");
}

/// Why: #10524 AC3 — the grant ends at exactly its TTL, on `Instant` alone
/// as well as on both clocks, and the ending is reported once.
/// Test: itself.
#[test]
fn grant_expires_at_ttl_with_injected_clock() {
    let ttl = TTL.as_secs() as i64;
    let clocks = Clocks::new();
    let cache = clocks.cache();
    cache.open(TTL, alive());
    assert!(fill(&cache, "K", "v"));
    clocks.both(ttl - 1);
    assert!(matches!(cache.check(), Status::Live { remaining, .. } if remaining == secs(1)));
    assert_eq!(served(&cache, "K").as_deref(), Some("v"));
    clocks.both(ttl);
    assert_eq!(served(&cache, "K"), None, "served at the TTL");
    assert_eq!(cache.check(), Status::Ended(DropReason::Expired));
    assert_eq!(cache.check(), Status::Off, "an ending was reported twice");
    assert_eq!(cache.count(), 0);

    // Wall clock held still; `Instant` alone reaches the TTL.
    let clocks = Clocks::new();
    let cache = clocks.cache();
    cache.open(TTL, alive());
    assert!(fill(&cache, "K", "v"));
    clocks.mono.store(ttl, Ordering::SeqCst);
    assert_eq!(served(&cache, "K"), None, "served past the TTL on Instant");
    assert_eq!(cache.check(), Status::Ended(DropReason::Expired));
}

/// Why: #10524 AC3 — `Instant` stops while the machine sleeps; a wall clock
/// that ran a sleep-sized gap ahead ends the grant as `sleep`.
/// Test: itself.
#[test]
fn grant_drops_on_a_sleep_gap() {
    let clocks = Clocks::new();
    let cache = clocks.cache();
    cache.open(TTL, alive());
    assert!(fill(&cache, "K", "v"));
    clocks
        .wall
        .store(SLEEP_GAP.as_secs() as i64, Ordering::SeqCst);
    assert_eq!(served(&cache, "K"), None, "served after a sleep");
    assert_eq!(cache.check(), Status::Ended(DropReason::Sleep));
    assert_eq!(cache.count(), 0);
}

/// Why: #10524 AC7 (clock error) — a wall clock that stepped back gives an
/// unknown age; the grant ends as `fault` and nothing is served.
/// Test: itself.
#[test]
fn grant_clock_error_denies() {
    let clocks = Clocks::new();
    let cache = clocks.cache();
    cache.open(TTL, alive());
    assert!(fill(&cache, "K", "v"));
    clocks.wall.store(-1, Ordering::SeqCst);
    assert_eq!(served(&cache, "K"), None, "served across a clock step");
    assert_eq!(cache.check(), Status::Ended(DropReason::Fault));
}

/// Why: #10524 AC3/AC7 — a watcher that dies after unlock drops the grant.
/// Test: itself.
#[test]
fn grant_watcher_death_drops_the_grant() {
    let clocks = Clocks::new();
    let cache = clocks.cache();
    let watcher = alive();
    cache.open(TTL, Arc::clone(&watcher));
    assert!(fill(&cache, "K", "v"));
    watcher.store(false, Ordering::SeqCst);
    assert_eq!(served(&cache, "K"), None, "served with a dead watcher");
    assert_eq!(cache.check(), Status::Ended(DropReason::Fault));
    assert_eq!(cache.count(), 0);
}

/// Why: #10524 AC7 (poisoned lock) — a panic while the map was held ends
/// the grant; no value is served and none is kept.
/// Test: itself.
#[test]
fn grant_poisoned_lock_misses_and_drops() {
    let clocks = Clocks::new();
    let cache = clocks.cache();
    cache.open(TTL, alive());
    assert!(fill(&cache, "K", "v"));
    let poison = catch_unwind(AssertUnwindSafe(|| {
        let _held = cache.inner.lock();
        panic!("poison the grant");
    }));
    assert!(poison.is_err());
    assert_eq!(served(&cache, "K"), None, "served from a poisoned map");
    assert_eq!(cache.count(), 0);
    assert_eq!(cache.check(), Status::Ended(DropReason::Fault));
}

/// Why: #10524 AC7 — a lock or a reopen while a read is in flight moves the
/// epoch, so the value that read returns is never kept.
/// Test: itself.
#[test]
fn grant_revoke_during_in_flight_read_stores_nothing() {
    let clocks = Clocks::new();
    let cache = clocks.cache();
    let kc = BackendId::keychain();
    cache.open(TTL, alive());
    let Lookup::Miss(epoch) = cache.lookup(&kc, None, &vault(), &key("K")) else {
        panic!("expected a miss");
    };
    assert!(cache.close(DropReason::Revoked, None));
    cache.open(TTL, alive());
    let kept = cache.store(
        &kc,
        None,
        &vault(),
        &key("K"),
        epoch,
        &SecretValue::new("old"),
    );
    assert!(!kept, "a read started under the revoked grant was kept");
    assert_eq!(served(&cache, "K"), None);
}

/// Why: #10524 AC2 — a value whose page cannot be locked is not kept, and
/// the next read goes back to the backend.
/// Test: itself.
#[test]
fn grant_mlock_refused_caches_nothing() {
    fn refuse(_: *const u8, _: usize) -> bool {
        false
    }
    let clocks = Clocks::new();
    let (now, wall) = clocks.pair();
    let cache = GrantCache::with_lock(now, wall, refuse);
    cache.open(TTL, alive());
    assert!(!fill(&cache, "K", "v"), "kept without a page lock");
    assert_eq!(served(&cache, "K"), None);
    assert_eq!(cache.count(), 0);
}

/// Why: #10524 — a CLI-backed value answers only the settings it was read
/// under, and unknown settings bypass the grant.
/// Test: itself.
#[test]
fn grant_cli_values_are_filed_under_their_settings() {
    let under = OpenedUnder::for_account;
    let (op, a, b) = (BackendId::onepassword(), under("a"), under("b"));
    let clocks = Clocks::new();
    let cache = clocks.cache();
    cache.open(TTL, alive());
    assert!(matches!(
        cache.lookup(&op, None, &vault(), &key("K")),
        Lookup::Off
    ));
    let Lookup::Miss(epoch) = cache.lookup(&op, Some(&a), &vault(), &key("K")) else {
        panic!("expected a miss");
    };
    assert!(cache.store(
        &op,
        Some(&a),
        &vault(),
        &key("K"),
        epoch,
        &SecretValue::new("v")
    ));
    assert!(matches!(
        cache.lookup(&op, Some(&a), &vault(), &key("K")),
        Lookup::Hit(_)
    ));
    assert!(matches!(
        cache.lookup(&op, Some(&b), &vault(), &key("K")),
        Lookup::Miss(_)
    ));
}

/// Why: #10524 AC1 — through [`SecretCache::read_through`], a Keychain key
/// is read from the backend once while a grant is live, every time without
/// one, and again after the grant ends; a write's eviction reaches it.
/// Test: itself.
#[test]
fn session_cache_reads_keychain_through_the_grant() {
    let clocks = Clocks::new();
    let (now, wall) = clocks.pair();
    let cache = SecretCache::with_clocks(Duration::from_secs(600), now, wall);
    let (kc, web, name) = (BackendId::keychain(), vault(), key("K"));
    let mut reads = 0;
    let mut read = |cache: &SecretCache| {
        let value = cache.read_through(&kc, None, &web, &name, || {
            reads += 1;
            Ok::<_, SecretsError>(Some(SecretValue::new("v")))
        });
        assert_eq!(value.unwrap().unwrap().expose(), "v");
    };
    read(&cache);
    read(&cache);
    cache.grant().open(TTL, alive());
    read(&cache);
    read(&cache);
    cache.evict(&web, &name);
    read(&cache);
    cache.grant().close(DropReason::Revoked, None);
    read(&cache);
    assert_eq!(
        reads, 5,
        "backend reads: 2 bare, 1 filled, 1 after evict, 1 after lock"
    );
}

/// Why: #10524 AC6 — drop reasons are a closed, snake_case vocabulary.
/// Test: itself.
#[test]
fn grant_drop_reasons_serialize_as_snake_case() {
    for (reason, wire) in [
        (DropReason::Expired, "\"expired\""),
        (DropReason::Sleep, "\"sleep\""),
        (DropReason::Logout, "\"logout\""),
        (DropReason::Revoked, "\"revoked\""),
        (DropReason::Fault, "\"fault\""),
    ] {
        assert_eq!(serde_json::to_string(&reason).unwrap(), wire);
    }
    let newer: DropReason = serde_json::from_str("\"reboot\"").unwrap();
    assert_eq!(newer, DropReason::Other);
}
