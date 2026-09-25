//! Fail-open check for the daemon's one credential read (#8236 item 7).
//!
//! Every store here is a fake and every credential is a literal written three
//! lines above the assertion that reads it. No test touches an OS keychain, a
//! launchd domain, a real `$HOME`, or the host's `.env.local`.
//!
//! What each test pins: the arm returns `None` AND says so at ERROR with the
//! variable name and the error kind. A downgrade of any arm — to a default
//! value, to a silent `None`, or to a different kind — fails here.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serial_test::serial;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use trusty_common::credentials::KeyStoreError;

use super::*;

/// Collected ERROR/WARN/INFO lines from one `with_default` scope.
#[derive(Default)]
struct Sink {
    /// One rendered line per event: level, then `name=value` per field.
    lines: Mutex<Vec<String>>,
}

/// A `tracing` layer that renders every event into [`Sink`].
struct CaptureLayer {
    /// Where rendered lines land.
    sink: Arc<Sink>,
}

/// Renders an event's fields into a flat string.
struct Fields {
    /// The line built so far.
    out: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.out, " {}={value:?}", field.name());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        use std::fmt::Write as _;
        let _ = write!(self.out, " {}={value}", field.name());
    }
}

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields {
            out: event.metadata().level().to_string(),
        };
        event.record(&mut fields);
        self.sink
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(fields.out);
    }
}

/// Run `f` with a capturing subscriber installed on this thread.
fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let sink = Arc::new(Sink::default());
    let subscriber = tracing_subscriber::registry().with(CaptureLayer {
        sink: Arc::clone(&sink),
    });
    let out = tracing::subscriber::with_default(subscriber, f);
    let lines = sink
        .lines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    (out, lines)
}

/// Assert exactly one ERROR line, naming `var` and `kind`.
fn assert_logged(lines: &[String], var: &str, kind: &str) {
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "expected one ERROR line, got {lines:?}");
    assert!(errors[0].contains(var), "{}", errors[0]);
    assert!(
        errors[0].contains(&format!("kind={kind}")),
        "expected kind={kind}: {}",
        errors[0]
    );
}

/// Removes an environment variable for a test's duration, restoring it after.
///
/// Why: the resolver's first tier is the process environment, so a host that
/// happens to export the variable under test would make every arm below answer
/// `Some` for a reason that has nothing to do with the code. Paired with
/// `#[serial]`, which is what makes the unsafe set/remove sound.
struct EnvVarGuard {
    /// The variable this guard owns.
    key: &'static str,
    /// Its value before the guard took it, if it had one.
    prev: Option<String>,
}

impl EnvVarGuard {
    /// Take `key` out of the environment until the guard drops.
    fn unset(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: every user of this guard is `#[serial]`, so no other thread
        // races the remove/restore. Restored in `Drop`.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prev }
    }

    /// Set `key` to `value` until the guard drops.
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: as `unset`.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: as the constructors.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// A store that answers instantly and holds nothing, counting its reads.
struct AbsentStore {
    /// How many reads reached the store.
    calls: Arc<AtomicUsize>,
}

impl KeyStore for AbsentStore {
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

/// A store whose read never returns — a SecurityAgent dialog, in a fake.
struct NeverReturns;

impl KeyStore for NeverReturns {
    fn get(&self, _provider: &str) -> Option<String> {
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

/// A store whose backend refuses — a locked or denied keychain.
struct RefusingStore;

impl KeyStore for RefusingStore {
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

/// Why: "nothing is configured" is the one failure an operator is allowed to
/// ignore, and it is also the one most easily turned into a silent `None` by a
/// later refactor. The ERROR line is what tells a stopped feature apart from a
/// feature nobody turned on; asserting on it means a downgrade to a quiet
/// return — or to any non-`None` default — fails here.
/// Test: this test.
#[test]
#[serial]
fn an_absent_secret_is_none_and_logs_absent() {
    let _guard = EnvVarGuard::unset("BITBUCKET_APP_PASSWORD");
    let calls = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(AbsentStore {
        calls: Arc::clone(&calls),
    });

    let (resolved, lines) = capture(|| {
        resolve_secret_with("BITBUCKET_APP_PASSWORD", store, Duration::from_millis(500))
    });

    assert!(resolved.is_none(), "an absent credential must not resolve");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the store was consulted once"
    );
    assert_logged(&lines, "BITBUCKET_APP_PASSWORD", "absent");
}

/// Why (#8236 item 6): under launchd the first read by a rebuilt binary parks
/// on a dialog. The daemon must come back `None` inside its bound and must say
/// TIMEOUT, not ABSENT — an operator told "absent" reinstates the plaintext
/// plist entry, which is this issue happening again. A downgrade of this arm to
/// a retry, a cached value, or the absent wording fails here.
/// Test: this test.
#[test]
#[serial]
fn a_store_timeout_is_none_and_logs_timeout() {
    let _guard = EnvVarGuard::unset("JIRA_API_TOKEN");
    let bound = Duration::from_millis(200);

    let started = std::time::Instant::now();
    let (resolved, lines) =
        capture(|| resolve_secret_with("JIRA_API_TOKEN", Arc::new(NeverReturns), bound));
    let waited = started.elapsed();

    assert!(resolved.is_none(), "a timed-out read must not resolve");
    assert!(
        waited < Duration::from_secs(2),
        "the caller waited {waited:?}"
    );
    assert_logged(&lines, "JIRA_API_TOKEN", "timeout");
}

/// Why: a refused store and an unconfigured one send an operator to two
/// different remedies. The kind is the only thing that separates them, so an
/// arm that collapsed every backend failure into "absent" — or, worse, into a
/// success with an empty value — fails here.
/// Test: this test.
#[test]
#[serial]
fn a_store_error_is_none_and_logs_the_kind() {
    let _guard = EnvVarGuard::unset("LINEAR_API_KEY");

    let (resolved, lines) = capture(|| {
        resolve_secret_with(
            "LINEAR_API_KEY",
            Arc::new(RefusingStore),
            Duration::from_millis(500),
        )
    });

    assert!(resolved.is_none(), "a refused store must not resolve");
    assert_logged(&lines, "LINEAR_API_KEY", "keyring-backend");
}

/// Why: `[llm] api_key_env` lets an operator name any variable, and an
/// unregistered one has no provider — so it has no store tier and no `.env`
/// tier either. Reinstating a file tier here is how a credential gets back into
/// a plaintext committed file, which is the same defect in a different file.
/// This pins that the store is NEVER consulted for such a name, and that the
/// process environment is the whole answer.
/// Test: this test.
#[test]
#[serial]
fn an_unregistered_variable_reads_only_the_process_environment() {
    let calls = Arc::new(AtomicUsize::new(0));
    let store = || {
        Arc::new(AbsentStore {
            calls: Arc::clone(&calls),
        })
    };

    let present = EnvVarGuard::set("TM_TEST_UNREGISTERED_8236", "from-the-process-env");
    let (resolved, lines) = capture(|| {
        resolve_secret_with(
            "TM_TEST_UNREGISTERED_8236",
            store(),
            Duration::from_millis(500),
        )
    });
    assert_eq!(resolved.as_deref(), Some("from-the-process-env"));
    assert!(lines.is_empty(), "a success must log nothing: {lines:?}");
    drop(present);

    let (resolved, lines) = capture(|| {
        resolve_secret_with(
            "TM_TEST_UNREGISTERED_8236",
            store(),
            Duration::from_millis(500),
        )
    });
    assert!(resolved.is_none(), "an unset variable must not resolve");
    assert_logged(&lines, "TM_TEST_UNREGISTERED_8236", "absent");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "an unregistered name must never reach the credential store"
    );
}
