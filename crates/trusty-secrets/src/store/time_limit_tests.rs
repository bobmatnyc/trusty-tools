//! Tests for [`TimeLimited`]: a backend call that never returns becomes
//! [`SecretsError::Timeout`] within the limit (#7524 P2-L7).
//!
//! Test: itself.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use tempfile::TempDir;

use super::*;
use crate::store::{MemoryBackend, NamesIndex, SecretStore};

/// The limit under test; the default is far too long for a unit test.
const LIMIT: Duration = Duration::from_millis(200);

/// How much later than [`LIMIT`] a call may return and still pass.
const MARGIN: Duration = Duration::from_secs(3);

fn names() -> (VaultName, SecretKey) {
    (
        VaultName::new("trusty/acme/web").unwrap(),
        SecretKey::new("API_KEY").unwrap(),
    )
}

/// A backend whose every call blocks forever, as a Keychain call does while
/// its access prompt stays open.
#[derive(Debug, Default)]
struct Hangs {
    calls: AtomicUsize,
}

impl Hangs {
    fn hang(&self) -> ! {
        self.calls.fetch_add(1, Ordering::SeqCst);
        loop {
            std::thread::park();
        }
    }
}

impl SecretBackend for Hangs {
    fn id(&self) -> BackendId {
        BackendId::from_static("hangs")
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }

    fn get(&self, _: &VaultName, _: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.hang()
    }

    fn set(&self, _: &VaultName, _: &SecretKey, _: &SecretValue) -> Result<(), SecretsError> {
        self.hang()
    }

    fn delete(&self, _: &VaultName, _: &SecretKey) -> Result<bool, SecretsError> {
        self.hang()
    }

    fn list_names(&self, _: &VaultName) -> Result<Vec<SecretKey>, SecretsError> {
        self.hang()
    }

    fn agents_may_use(&self, _: &VaultName, _: &SecretKey) -> Result<bool, SecretsError> {
        self.hang()
    }

    fn set_agents_may_use(
        &self,
        _: &VaultName,
        _: &SecretKey,
        _: bool,
    ) -> Result<(), SecretsError> {
        self.hang()
    }
}

/// Run `f` on a watchdog thread. Panics, rather than hanging the suite, if
/// it has not returned within [`LIMIT`] plus [`MARGIN`].
fn returns_in_time<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> (T, Duration) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let started = Instant::now();
        let out = f();
        let _ = tx.send((out, started.elapsed()));
    });
    let bound = LIMIT + MARGIN;
    rx.recv_timeout(bound)
        .unwrap_or_else(|_| panic!("the backend call did not return within {bound:?}: it hangs"))
}

type Op = fn(&TimeLimited, &VaultName, &SecretKey) -> Result<(), SecretsError>;

/// Every [`SecretBackend`] call, by the operation name its timeout carries.
const OPS: [(&str, Op); 7] = [
    ("get", |b, v, k| b.get(v, k).map(drop)),
    ("set", |b, v, k| {
        b.set(v, k, &SecretValue::new("sk-fake-7524"))
    }),
    ("delete", |b, v, k| b.delete(v, k).map(drop)),
    ("list_names", |b, v, _| b.list_names(v).map(drop)),
    ("agents_may_use", |b, v, k| b.agents_may_use(v, k).map(drop)),
    ("set_agents_may_use", |b, v, k| {
        b.set_agents_may_use(v, k, true)
    }),
    ("set_agents_may_use", |b, v, k| {
        b.set_agents_may_use(v, k, false)
    }),
];

/// Why: #7524 P2-L7 — a Keychain call blocked on an unanswered prompt held
/// its caller forever. A read that timed out must not read as a miss, and a
/// write or delete must not report success.
/// What: each operation over a backend that never returns comes back within
/// the limit plus a margin, as `Timeout` naming that operation, the backend,
/// the vault and the key. Without the limit the watchdog fails the test.
/// Test: itself.
#[test]
fn time_limited_call_that_never_returns_times_out_on_every_operation() {
    let (vault, key) = names();
    let hangs = Arc::new(Hangs::default());
    let limited = TimeLimited::new(Arc::clone(&hangs) as Arc<dyn SecretBackend>, LIMIT);
    for (name, op) in OPS {
        let (b, v, k) = (limited.clone(), vault.clone(), key.clone());
        let (result, took) = returns_in_time(move || op(&b, &v, &k));
        match result {
            Err(SecretsError::Timeout {
                backend,
                operation,
                vault,
                key,
                waited,
            }) => {
                assert_eq!(operation, name);
                assert_eq!(backend, "hangs");
                assert_eq!(vault, "trusty/acme/web");
                let expected_key = if name == "list_names" {
                    "(none)"
                } else {
                    "API_KEY"
                };
                assert_eq!(key, expected_key);
                assert_eq!(waited, LIMIT);
            }
            other => panic!("{name}: expected Timeout, got {other:?}"),
        }
        assert!(took >= LIMIT, "{name} returned before its limit: {took:?}");
    }
    assert_eq!(hangs.calls.load(Ordering::SeqCst), OPS.len());
}

/// Why: #7524 P2-L7 — `list` reads each key's flag item (#9070) and `set`
/// writes one, so both reach the hang through the store.
/// What: a store over the never-returning backend fails `list` and `set`
/// with `Timeout`, and the timed-out `set` leaves no index row.
/// Test: itself.
#[test]
fn time_limited_store_list_and_set_time_out_and_set_leaves_no_row() {
    let (vault, key) = names();
    let tmp = TempDir::new().unwrap();
    let index = || NamesIndex::at(tmp.path().join("index"));
    let seeded = SecretStore::new(Arc::new(MemoryBackend::new()), index());
    seeded
        .set(&vault, &key, &SecretValue::new("sk-fake-7524"))
        .unwrap();

    let hangs: Arc<dyn SecretBackend> = Arc::new(Hangs::default());
    let store = SecretStore::new(Arc::new(TimeLimited::new(hangs, LIMIT)), index());
    let (v, fresh) = (vault.clone(), SecretKey::new("NEW_KEY").unwrap());
    let ((listed, set), _) = returns_in_time(move || {
        let listed = store.list(&v).map(drop);
        let set = store.set(&v, &fresh, &SecretValue::new("sk-fake-7524"));
        (listed, set.map(drop))
    });
    assert!(
        matches!(
            listed,
            Err(SecretsError::Timeout {
                operation: "agents_may_use",
                ..
            })
        ),
        "{listed:?}"
    );
    assert!(matches!(set, Err(SecretsError::Timeout { .. })), "{set:?}");
    let rows = index().list(&vault).unwrap();
    assert_eq!(rows.len(), 1, "a timed-out set wrote an index row");
    assert_eq!(rows[0].name, key);
}

/// Why: the limit must not change what a call that returns in time reports.
/// What: values, misses, delete results, flags and errors pass through
/// unchanged.
/// Test: itself.
#[test]
fn time_limited_call_passes_results_and_errors_through() {
    let (vault, key) = names();
    let limited = TimeLimited::new(Arc::new(MemoryBackend::new()), MARGIN);
    assert_eq!(limited.id().as_str(), "memory");
    assert!(limited.get(&vault, &key).unwrap().is_none());
    limited
        .set(&vault, &key, &SecretValue::new("sk-fake-7524"))
        .unwrap();
    assert_eq!(
        limited.get(&vault, &key).unwrap().unwrap().expose(),
        "sk-fake-7524"
    );
    limited.set_agents_may_use(&vault, &key, true).unwrap();
    assert!(limited.agents_may_use(&vault, &key).unwrap());
    assert!(limited.delete(&vault, &key).unwrap());
    assert!(!limited.delete(&vault, &key).unwrap());
    let err = limited.list_names(&vault).unwrap_err();
    assert!(
        matches!(
            err,
            SecretsError::Unsupported {
                operation: "list_names",
                ..
            }
        ),
        "{err:?}"
    );
}

/// Why: #7524 P2-M1 and P2-L7 — a call must not start once the server's
/// request deadline has passed, or a write could land after the caller was
/// told the request failed.
/// What: under a deadline that has run out, the call is a `Timeout` and the
/// wrapped backend is never called.
/// Test: itself.
#[test]
#[cfg(any(feature = "server", feature = "cli-backends"))]
fn time_limited_call_is_not_started_after_the_request_deadline() {
    let (vault, key) = names();
    let hangs = Arc::new(Hangs::default());
    let limited = TimeLimited::new(Arc::clone(&hangs) as Arc<dyn SecretBackend>, LIMIT);
    let value = SecretValue::new("sk-fake-7524");
    let (result, _) = returns_in_time(move || {
        crate::store::deadline::within(Instant::now(), || limited.set(&vault, &key, &value))
    });
    assert!(
        matches!(result, Err(SecretsError::Timeout { operation: "set", waited, .. }) if waited.is_zero()),
        "{result:?}"
    );
    assert_eq!(
        hangs.calls.load(Ordering::SeqCst),
        0,
        "the call was started"
    );
}
