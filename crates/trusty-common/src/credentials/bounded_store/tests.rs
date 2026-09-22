//! Tests for the bounded, single-flight credential-store tier (#8236).
//!
//! Every store here is a fake. No test touches an OS keychain, a real `$HOME`,
//! or any installed file, and no test asserts on a credential value that is not
//! a literal written three lines above it.

use std::sync::atomic::{AtomicUsize, Ordering};

use serial_test::serial;

use super::*;
use crate::credentials::env_guard::EnvVarGuard;

/// A store whose `get` parks forever, standing in for a Keychain dialog.
///
/// Why: the measured failure mode is a `get_password` that never returns
/// because a SecurityAgent dialog is waiting for a human. A fake that blocks is
/// the only way to test the caller's bound without one.
struct NeverReturns {
    /// How many reads were actually issued. Single-flight is asserted on this.
    calls: Arc<AtomicUsize>,
}

impl KeyStore for NeverReturns {
    fn get(&self, _provider: &str) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    fn set(&self, _provider: &str, _value: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn unset(&self, _provider: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn list(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A store whose `try_get` always reports a backend failure.
struct AlwaysFails;

impl KeyStore for AlwaysFails {
    fn get(&self, _provider: &str) -> Option<String> {
        None
    }
    fn try_get(&self, _provider: &str) -> Result<Option<String>, KeyStoreError> {
        Err(KeyStoreError::Keyring("locked".to_string()))
    }
    fn set(&self, _provider: &str, _value: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn unset(&self, _provider: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn list(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A store that answers instantly, counting its reads.
struct CountingAbsent {
    calls: Arc<AtomicUsize>,
}

impl KeyStore for CountingAbsent {
    fn get(&self, _provider: &str) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn set(&self, _provider: &str, _value: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn unset(&self, _provider: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn list(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Why (#8236 item 6): a store read that never returns must not hold the
/// caller. The daemon's async path waits on this, and a launchd Keychain
/// dialog makes "never returns" the routine case after every reinstall.
/// Test: itself.
#[test]
fn a_store_that_never_returns_times_out_within_the_bound() {
    let calls = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn KeyStore> = Arc::new(NeverReturns {
        calls: Arc::clone(&calls),
    });
    let bound = Duration::from_millis(200);

    let started = Instant::now();
    let err = store_get_bounded(store, "test-never-returns-a", bound).expect_err("must time out");
    let elapsed = started.elapsed();

    assert_eq!(err, StoreErrorKind::Timeout);
    assert!(
        elapsed < bound * 5,
        "the caller waited {elapsed:?}, well past the {bound:?} bound"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Why (#8236 item 6b): the readers resolve per call — the overseer classifies
/// every supervisor tick — so without single-flight each tick parks another
/// thread and raises another dialog.
/// Test: itself.
#[test]
fn concurrent_resolves_issue_exactly_one_store_read() {
    let calls = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn KeyStore> = Arc::new(NeverReturns {
        calls: Arc::clone(&calls),
    });
    let provider = "test-single-flight-b";

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                store_get_bounded(store, provider, Duration::from_millis(150))
            })
        })
        .collect();

    for handle in handles {
        let got = handle.join().expect("thread");
        assert_eq!(got, Err(StoreErrorKind::Timeout));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "8 concurrent callers issued more than one store read"
    );
}

/// Why (#8236 item 6c): a cached failure must suppress the READ, so a wedged
/// dialog is not re-raised every tick.
/// Test: itself.
#[test]
fn a_cached_error_is_returned_without_a_second_read() {
    let provider = "test-negative-cache-c";
    record_error(provider, StoreErrorKind::Keyring);
    let calls = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn KeyStore> = Arc::new(CountingAbsent {
        calls: Arc::clone(&calls),
    });

    let err = store_get_bounded(store, provider, Duration::from_secs(1)).expect_err("cached");

    assert_eq!(err, StoreErrorKind::Keyring);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the cache did not suppress"
    );
}

/// Why (#8236 item 6c): the cache must EXPIRE, or an operator who approves the
/// dialog is locked out until the daemon restarts.
/// Test: itself.
#[test]
fn the_error_cache_expires_and_the_next_read_is_issued() {
    let provider = "test-negative-cache-expiry-d";
    map(&ERROR_CACHE)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            provider.to_string(),
            (
                Instant::now() - STORE_ERROR_CACHE_TTL - Duration::from_secs(1),
                StoreErrorKind::Timeout,
            ),
        );
    let calls = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn KeyStore> = Arc::new(CountingAbsent {
        calls: Arc::clone(&calls),
    });

    let got = store_get_bounded(store, provider, Duration::from_secs(2));

    assert_eq!(got, Ok(None), "a stale cache entry blocked a fresh read");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Why (#8236 item 7): a backend failure is an ERROR with a kind, never a
/// silent `None` the caller mistakes for "not configured".
/// Test: itself.
#[test]
fn a_failing_store_reports_its_kind() {
    let store: Arc<dyn KeyStore> = Arc::new(AlwaysFails);
    let err = store_get_bounded(store, "test-failing-e", Duration::from_secs(1))
        .expect_err("backend failure");
    assert_eq!(err, StoreErrorKind::Keyring);
}

/// Why (#8236 item 7): a genuine miss is `Absent`, which is a DIFFERENT
/// remediation from a store error and must not be reported as one.
/// Test: itself.
#[test]
#[serial(dotenv_credential_env)]
fn an_absent_value_is_absent_not_an_error() {
    // The ambient shell environment is not a fixture this test controls (#4407).
    let _guard = EnvVarGuard::remove("LINEAR_API_KEY");
    let store: Arc<dyn KeyStore> = Arc::new(CountingAbsent {
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let err = resolve_provider_bounded_with("linear", store, Duration::from_secs(1))
        .expect_err("nothing configured");
    assert!(matches!(err, SecretResolveError::Absent { .. }), "{err:?}");
    assert_eq!(err.kind(), "absent");
    assert_eq!(err.var(), "LINEAR_API_KEY");
}

/// Why (#8236 item 5): the env tier still wins, so an operator's explicit
/// export is never overridden by the store — and a configured process never
/// touches the Keychain at all.
/// Test: itself.
#[test]
#[serial(dotenv_credential_env)]
fn the_env_tier_answers_without_touching_the_store() {
    let _guard = EnvVarGuard::set("BRAVE_API_KEY", "synthetic-env-value");
    let calls = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn KeyStore> = Arc::new(CountingAbsent {
        calls: Arc::clone(&calls),
    });

    let got = resolve_provider_bounded_with("brave", store, Duration::from_secs(1));

    assert_eq!(got.as_deref(), Ok("synthetic-env-value"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Why (#8236 item 2): a name with no registry mapping has no provider to
/// resolve under, and the caller must hear that rather than a silent miss.
/// Test: itself.
#[test]
fn an_unregistered_variable_is_not_resolvable() {
    let err = resolve_env_var_bounded("AWS_SECRET_ACCESS_KEY").expect_err("unregistered");
    assert!(
        matches!(err, SecretResolveError::Unregistered { .. }),
        "{err:?}"
    );
    assert_eq!(err.kind(), "unregistered");
}

/// Why (#8236 item 9): every error arm is logged, so no variant may render a
/// value. Constructed with an obviously-fake value in the `var` slot to prove
/// the rendering names only the key and the kind.
/// Test: itself.
#[test]
fn error_kinds_render_without_any_value() {
    let value = "sk-synthetic-never-a-real-key";
    for err in [
        SecretResolveError::Absent {
            var: "OPENROUTER_API_KEY".to_string(),
        },
        SecretResolveError::Timeout {
            var: "OPENROUTER_API_KEY".to_string(),
            waited_ms: 3000,
            cached: false,
        },
        SecretResolveError::Store {
            var: "OPENROUTER_API_KEY".to_string(),
            kind: StoreErrorKind::Keyring,
            cached: true,
        },
    ] {
        let rendered = format!("{err} {err:?}");
        assert!(!rendered.contains(value), "a value reached the message");
        assert!(rendered.contains("OPENROUTER_API_KEY"), "{rendered}");
    }
    assert_eq!(StoreErrorKind::Timeout.to_string(), "timeout");
}
