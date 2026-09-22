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

    assert_eq!(err.kind, StoreErrorKind::Timeout);
    assert!(!err.cached, "a fresh read must not report itself cached");
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
        assert_eq!(got, Err(StoreFailure::fresh(StoreErrorKind::Timeout)));
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

    assert_eq!(err.kind, StoreErrorKind::Keyring);
    assert!(err.cached, "the cache answered but did not say so");
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
    assert_eq!(err.kind, StoreErrorKind::Keyring);
    assert!(!err.cached);
}

/// Why (#8236 item 7): a genuine miss is `Absent`, which is a DIFFERENT
/// remediation from a store error and must not be reported as one.
/// Test: itself.
#[test]
#[serial(dotenv_credential_env)]
fn an_absent_value_is_absent_not_an_error() {
    // #7253: `#[serial]` and `ENV_LOCK` exclude nothing of each other, so an
    // env-mutating test takes BOTH. Locked first, dropped last.
    let _env = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    // #7253: see the sibling test above — both locks, in this order.
    let _env = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// A store that answers with a value only after `delay`, standing in for a
/// dialog a human approves seconds after the caller gave up.
struct SlowValue {
    delay: Duration,
    value: String,
    calls: Arc<AtomicUsize>,
}

impl KeyStore for SlowValue {
    fn get(&self, _provider: &str) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        Some(self.value.clone())
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

/// Block until `provider` has no outstanding flight, or the bound elapses.
///
/// Why: the reader is DETACHED, so there is no handle to join. Polling the
/// flight map is the only way to observe it landing, and a fixed sleep would
/// either be flaky or slow.
fn await_reader_done(provider: &str, bound: Duration) {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        let outstanding = map(&INFLIGHT)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(provider);
        if !outstanding {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the detached reader never finished within {bound:?}");
}

/// Why (#8236): the caller-side timeout caches a `Timeout` for 45 seconds, and
/// the whole point of detaching the reader is that a LATE approval still
/// counts. If a successful read does not retire that entry, every caller in the
/// window is answered from the stale error and the obtained value is discarded
/// — the module's "an operator who approves the dialog is picked up on the next
/// resolve" promise, broken.
/// Test: itself.
#[test]
fn a_late_success_retires_the_cached_timeout() {
    let provider = "test-late-approval-f";
    let calls = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn KeyStore> = Arc::new(SlowValue {
        delay: Duration::from_millis(250),
        value: "synthetic-late-approved-value".to_string(),
        calls: Arc::clone(&calls),
    });

    // The caller gives up while the read is still parked, exactly as it does
    // when a SecurityAgent dialog is on screen.
    let err = store_get_bounded(Arc::clone(&store), provider, Duration::from_millis(30))
        .expect_err("the caller must give up first");
    assert_eq!(err.kind, StoreErrorKind::Timeout);

    // ...and the detached read lands afterwards, with the value.
    await_reader_done(provider, Duration::from_secs(5));

    let got = store_get_bounded(store, provider, Duration::from_secs(5));

    assert_eq!(
        got,
        Ok(Some("synthetic-late-approved-value".to_string())),
        "the stale Timeout entry outlived the successful read that cleared it"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the second read was suppressed"
    );
}

/// Why (#8236): `cached` told every caller `false`, because the flag was a
/// literal at the construction site rather than something the read reported.
/// The two states need different remediations — "approve the dialog now" for a
/// fresh failure, "the suppression window is still open" for a cached one — so
/// a flag that is structurally always `false` is worse than none.
/// Test: itself.
#[test]
fn a_store_failure_is_fresh_first_and_cached_second() {
    // Unregistered on purpose: the provider key doubles as the variable name,
    // so this reads nothing an operator's environment could be holding.
    let provider = "test-cached-flag-g";
    let store: Arc<dyn KeyStore> = Arc::new(AlwaysFails);

    let fresh = resolve_provider_bounded_with(provider, Arc::clone(&store), Duration::from_secs(1))
        .expect_err("the backend refuses");
    let repeat = resolve_provider_bounded_with(provider, store, Duration::from_secs(1))
        .expect_err("the negative cache answers");

    assert!(
        matches!(
            fresh,
            SecretResolveError::Store {
                kind: StoreErrorKind::Keyring,
                cached: false,
                ..
            }
        ),
        "a freshly-read failure claimed to be cached: {fresh:?}"
    );
    assert!(
        matches!(cached_flag(&repeat), Some(true)),
        "a cache-served failure did not report itself cached: {repeat:?}"
    );
}

/// The `cached` flag of a `Store`/`Timeout` error, when it has one.
fn cached_flag(err: &SecretResolveError) -> Option<bool> {
    match err {
        SecretResolveError::Store { cached, .. } | SecretResolveError::Timeout { cached, .. } => {
            Some(*cached)
        }
        _ => None,
    }
}

/// A one-shot handshake between the test thread and a parked reader.
///
/// Why: the race the last test forces is a two-party ordering, and a sleep on
/// either side would make it a coin flip rather than a proof.
struct Gate {
    /// False until the other party opens it. Never closes again.
    open: Mutex<bool>,
    /// Signalled once, when `open` flips.
    changed: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            changed: Condvar::new(),
        })
    }

    fn open(&self) {
        *self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*open {
            open = self
                .changed
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// Installs [`PARK_BEFORE_PUBLISH`] for one provider; removes it on drop.
///
/// Why: the hook is process-global while the suite is multi-threaded, so it
/// fires for exactly one provider key and must not survive a panicking test.
struct InstalledParkHook;

impl InstalledParkHook {
    fn install(provider: &'static str, parked: Arc<Gate>, release: Arc<Gate>) -> Self {
        let hook: ParkHook = Arc::new(move |p: &str| {
            if p != provider {
                return;
            }
            parked.open();
            release.wait();
        });
        *PARK_BEFORE_PUBLISH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
        Self
    }
}

impl Drop for InstalledParkHook {
    fn drop(&mut self) {
        *PARK_BEFORE_PUBLISH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Why (#8236): `finish` wrote [`ERROR_CACHE`] and published the outcome in two
/// separate critical sections, and the caller's give-up path dropped
/// `flight.outcome` before recording its `Timeout`. A reader preempted between
/// those two steps let the caller write a `Timeout` AFTER the reader's
/// `clear_error`, so the negative cache suppressed every read for the full 45 s
/// TTL although the value had already landed — the exact defect the
/// late-approval fix was meant to close.
/// `a_late_success_retires_the_cached_timeout` cannot see it: `await_reader_done`
/// sequences the two strictly.
/// Test: itself.
#[test]
fn a_timed_out_caller_cannot_cache_behind_a_publishing_reader() {
    let provider = "test-publish-race-h";
    let value = "synthetic-race-approved-value";
    clear_error(provider);

    // The reader signals `parked` once it has produced its value and made its
    // cache decision, then blocks until the test opens `release`.
    let parked = Gate::new();
    let release = Gate::new();
    let _hook = InstalledParkHook::install(provider, Arc::clone(&parked), Arc::clone(&release));

    let calls = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn KeyStore> = Arc::new(SlowValue {
        delay: Duration::ZERO,
        value: value.to_string(),
        calls: Arc::clone(&calls),
    });
    let bound = Duration::from_millis(120);
    let caller = std::thread::spawn(move || store_get_bounded(store, provider, bound));

    parked.wait();
    // The caller's whole bound elapses with the reader stopped mid-publish.
    std::thread::sleep(bound * 2);
    release.open();

    await_reader_done(provider, Duration::from_secs(5));
    let got = caller.join().expect("caller thread");

    assert_eq!(
        cached_error(provider),
        None,
        "the timed-out caller cached a Timeout behind the reader's clear_error, \
         suppressing reads for the full TTL although the value was published"
    );
    assert_eq!(
        got,
        Ok(Some(value.to_string())),
        "the caller must observe the outcome the reader published under the \
         same lock, not its own Timeout"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
