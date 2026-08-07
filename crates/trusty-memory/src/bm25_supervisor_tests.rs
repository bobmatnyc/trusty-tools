//! Unit tests for [`super::Bm25Supervisor`].
//!
//! Why: split out of `bm25_supervisor.rs` so the production module stays under
//! the 500-SLOC cap. Since #5089 promoted the state machine into
//! `trusty-common`, this file covers only what is still BM25's: the env knobs,
//! the external-mode opt-out, socket adoption at BM25's canonical path,
//! idempotent shutdown, and the patience-vs-flush relationship that binds this
//! service's timeouts to `trusty-bm25-daemon`'s own budget. The limits, the
//! probe classification and the eviction bookkeeping moved to
//! `trusty-common`'s `uds/supervisor/tests.rs` with the code they cover.
//! Test: this *is* the test file.

use super::*;
use tokio::sync::Mutex as TokioMutex;

/// Process-wide lock serialising every test that touches `TRUSTY_BM25_*`.
///
/// Why: cargo runs tests in parallel inside the same process, so two tests
/// mutating the same env var race each other. A `tokio::sync::Mutex` (not
/// `std::sync::Mutex`) because the guard is held across `.await` calls in
/// `ensure_running`; a std guard held across an await blocks the runtime and is
/// flagged by `clippy::await_holding_lock`.
/// Test: used by every env-mutating test below.
fn env_lock() -> std::sync::Arc<TokioMutex<()>> {
    static LOCK: std::sync::OnceLock<std::sync::Arc<TokioMutex<()>>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Arc::new(TokioMutex::new(())))
        .clone()
}

/// Why: the supervisor must start with an empty map so the first
/// `ensure_running` call always takes the cold-path branch.
/// Test: this test itself.
#[tokio::test]
async fn supervisor_starts_empty() {
    let sup = Bm25Supervisor::new();
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: `Default::default()` must behave like `new()`. Catches a regression
/// where someone adds state to `new` and forgets to mirror it on `Default`.
/// Test: this test itself.
#[tokio::test]
async fn supervisor_default_matches_new() {
    let sup: Bm25Supervisor = Default::default();
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: in external-management mode `ensure_running` must NOT spawn anything; it
/// must hand back the socket path the caller would have ended up at. Pinning it
/// here guards against a regression that fires off a child with the env var set.
/// What: sets `TRUSTY_BM25_EXTERNAL=1` and calls `ensure_running` against a
/// definitely-unused palace and a nonexistent data dir.
/// Test: this test itself.
#[tokio::test]
async fn external_mode_skips_spawn() {
    let lock = env_lock();
    let _env = lock.lock().await;
    let _guard = EnvGuard::set(ENV_EXTERNAL_BM25, "1");
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Bm25Supervisor::new();
    // Short palace name so the resolved socket path stays well under the
    // kernel's `sun_path` limit (~104 bytes on macOS).
    let palace = "ext-skip";
    let path = sup
        .ensure_running(palace, tmp.path())
        .await
        .expect("external mode must return socket path without spawning");
    assert_eq!(path, socket_path_for_palace(palace));
    assert_eq!(
        sup.supervised_count().await,
        0,
        "external mode must not register a child"
    );
}

/// Why: if some other process is already serving BM25's canonical socket path
/// — a stale daemon from a previous run, or an operator-managed launchd job that
/// forgot `TRUSTY_BM25_EXTERNAL` — spawning a second daemon would EADDRINUSE.
/// The supervisor must adopt the existing socket.
/// What: binds a listener the way the real daemon does (`bind_hardened`, so the
/// socket passes the adoption-time verification #5089 added) at the canonical
/// path for a test palace, then asserts no child is tracked.
/// Test: this test itself.
#[tokio::test]
async fn already_running_skips_spawn() {
    let lock = env_lock();
    let _env = lock.lock().await;
    // Don't pick up an external-mode flag from a sibling test that ran first.
    let _g = EnvGuard::remove(ENV_EXTERNAL_BM25);
    // The canonical path is `$TMPDIR/trusty-<uid>/trusty-bm25-<palace>.sock`,
    // and macOS' `$TMPDIR` is already long, so keep the palace fragment short.
    let palace = format!("a{:x}", std::process::id() & 0xffff);
    let socket = socket_path_for_palace(&palace);
    let _ = std::fs::remove_file(&socket);
    // #5099: bind the way the real daemon does. A bare `UnixListener::bind`
    // fails with ENOENT now that the canonical path sits inside a uid-keyed
    // directory that only `bind_hardened` creates.
    let listener =
        trusty_common::uds::bind_hardened(&socket).expect("bind dummy listener at canonical path");

    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Bm25Supervisor::new();
    let path = sup
        .ensure_running(&palace, tmp.path())
        .await
        .expect("ensure_running must adopt existing socket");
    assert_eq!(path, socket);
    assert_eq!(
        sup.supervised_count().await,
        0,
        "adoption path must not register a child"
    );

    drop(listener);
    let _ = std::fs::remove_file(&socket);
}

/// Why: `shutdown` on a fresh supervisor must not panic, error, or log anything
/// alarming. Operators will call it at exit even when no palace has touched BM25.
/// Test: this test itself.
#[tokio::test]
async fn shutdown_with_no_children_is_noop() {
    let sup = Bm25Supervisor::new();
    sup.shutdown().await;
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: `Bm25Supervisor` is shared via `Arc` and must be `Send + Sync` so it can
/// be cloned into background tasks and async handlers.
/// Test: this test itself.
#[test]
fn supervisor_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Bm25Supervisor>();
}

/// RAII guard for serialised env-var mutation in tests.
///
/// Why: cargo test runs tests in the same process by default, so
/// `std::env::set_var` mutations leak between tests unless restored on drop.
/// What: captures the prior value on construction, restores or removes on drop.
/// Test: used by every env-touching test in this module.
struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: test-only env mutation; each test takes `env_lock` before
        // mutating, and the Drop impl restores on scope exit.
        unsafe { std::env::set_var(key, value) }
        Self {
            key: key.to_string(),
            prev,
        }
    }

    fn remove(key: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: same invariant as `set`.
        unsafe { std::env::remove_var(key) }
        Self {
            key: key.to_string(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test teardown; the inverse of the mutation in `set`.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}

// ── Bounded process model: the knobs that stayed here (#2845 / #2846) ──────

/// Why: the default has to be a small number, because the failure it prevents is
/// one `memory_recall_all` leaving ~99 resident subprocesses. A regression that
/// quietly widened it would restore that failure without changing a line of
/// enforcement logic.
/// Test: this test itself.
#[test]
fn default_cap_is_three() {
    assert_eq!(DEFAULT_MAX_LIVE_DAEMONS, 3);
    assert_eq!(Bm25Supervisor::new().max_live(), 3);
}

/// Why: the cap is the knob an operator reaches for when their fan-out is
/// genuinely wider than the default working set. A typo must not silently
/// disable it.
/// What: exercises the valid override, the zero case, and the garbage case.
/// Test: this test itself.
#[tokio::test]
async fn max_live_daemons_honours_env_override() {
    let lock = env_lock();
    let _env = lock.lock().await;

    let _g = EnvGuard::set(ENV_MAX_DAEMONS, "7");
    assert_eq!(max_live_from_env(), 7);

    let _g = EnvGuard::set(ENV_MAX_DAEMONS, "0");
    assert_eq!(max_live_from_env(), DEFAULT_MAX_LIVE_DAEMONS);

    let _g = EnvGuard::set(ENV_MAX_DAEMONS, "banana");
    assert_eq!(max_live_from_env(), DEFAULT_MAX_LIVE_DAEMONS);

    let _g = EnvGuard::remove(ENV_MAX_DAEMONS);
    assert_eq!(max_live_from_env(), DEFAULT_MAX_LIVE_DAEMONS);
}

/// Why (#2846): `0` must be the documented, working way to turn RSS enforcement
/// off — distinct from the tightest possible ceiling. If those two collapsed
/// onto one value, an operator disabling the limit would instead get a
/// supervisor that reaps every daemon it spawns.
/// Test: this test itself.
#[tokio::test]
async fn rss_limit_honours_env_override() {
    let lock = env_lock();
    let _env = lock.lock().await;

    let _g = EnvGuard::set(ENV_RSS_LIMIT_MB, "1024");
    assert_eq!(rss_limit_from_env(), Some(1024));

    let _g = EnvGuard::set(ENV_RSS_LIMIT_MB, "0");
    assert_eq!(rss_limit_from_env(), None, "0 must disable, not reap-all");

    let _g = EnvGuard::set(ENV_RSS_LIMIT_MB, "lots");
    assert_eq!(rss_limit_from_env(), Some(DEFAULT_RSS_LIMIT_MB));

    let _g = EnvGuard::remove(ENV_RSS_LIMIT_MB);
    assert_eq!(rss_limit_from_env(), Some(DEFAULT_RSS_LIMIT_MB));
}

/// Why: a cap of 0 would mean "never keep a daemon", making every
/// `ensure_running` spawn-and-immediately-reap. Clamping to 1 turns an
/// operator's misconfiguration into a tight limit rather than a livelock.
/// Test: this test itself.
#[test]
fn cap_is_clamped_to_at_least_one() {
    assert_eq!(Bm25Supervisor::with_limits(0, None).max_live(), 1);
}

/// Why: the RSS ceiling must reach the shared supervisor rather than being
/// silently dropped by the wrapper. A ceiling that never arrives is #2846
/// again — declared, never compared.
/// Test: this test itself.
#[test]
fn explicit_limits_reach_the_shared_supervisor() {
    let sup = Bm25Supervisor::with_limits(9, Some(42));
    assert_eq!(sup.max_live(), 9);
    assert_eq!(sup.rss_limit_mb(), Some(42));
    assert_eq!(
        Bm25Supervisor::with_limits(9, None).rss_limit_mb(),
        None,
        "disabled enforcement must survive the wrapper too"
    );
}

/// Why (#5048 review, #5085, #5089): the supervisor's SIGTERM patience must
/// exceed the daemon's own shutdown-flush budget with margin. At an equal budget
/// the SIGKILL lands inside the flush the durability fix added and the open
/// write window is lost. The strict inequality is now enforced at compile time
/// by [`super::BM25_TIMEOUTS`] — `ServiceTimeouts::new` is a `const fn` whose
/// assert fires during const evaluation — so what this test still owes is the
/// MARGIN, which no type can express: signal delivery, socket cleanup and
/// process exit all have to fit on top of the flush itself.
/// What: asserts at least 2 s of headroom over `trusty_bm25_daemon`'s real
/// `SHUTDOWN_FLUSH_TIMEOUT`, imported rather than restated — a hardcoded copy
/// stays equal to itself while the daemon's budget drifts, so it could never
/// detect the drift it exists to name.
/// Test: this test itself.
#[test]
fn sigterm_patience_exceeds_the_daemon_flush_budget() {
    let daemon_flush_budget = trusty_bm25_daemon::SHUTDOWN_FLUSH_TIMEOUT;
    assert_eq!(
        BM25_TIMEOUTS.shutdown_flush, daemon_flush_budget,
        "the supervisor must be configured with the daemon's REAL flush budget, \
         or the compile-time guard checks the wrong number"
    );
    assert!(
        BM25_TIMEOUTS.sigterm_patience > daemon_flush_budget,
        "the supervisor must outwait the daemon's flush: {:?} vs {daemon_flush_budget:?}",
        BM25_TIMEOUTS.sigterm_patience
    );
    assert!(
        BM25_TIMEOUTS.sigterm_patience - daemon_flush_budget >= Duration::from_secs(2),
        "leave room for signal delivery, socket cleanup and process exit on top \
         of the flush itself"
    );
}

/// Why (#5089): the spawn budget is BM25's, not supervision's — 3 s is justified
/// by BM25 having no model to load, against 30 s for the embedder. A promotion
/// that dropped it on the floor and inherited someone else's default would fail
/// as a spawn timeout that looks like a broken binary.
/// Test: this test itself.
#[test]
fn spawn_probe_budget_reaches_the_shared_supervisor() {
    assert_eq!(BM25_TIMEOUTS.spawn_probe, Duration::from_millis(3000));
}
