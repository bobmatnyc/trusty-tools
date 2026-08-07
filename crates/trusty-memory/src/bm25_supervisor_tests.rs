//! Unit tests for the [`super::Bm25Supervisor`] bounded process model.
//!
//! Why: split out of `bm25_supervisor.rs` so the production module stays under
//! the 500-SLOC cap once the daemon cap, LRU reaping, and RSS enforcement
//! landed. Wired back in via `#[path] mod tests;`, the same way
//! `worker_liveness_tests.rs` is.
//! What: covers the external-mode opt-out, socket adoption, idempotent
//! shutdown, the socket probe, and the bounded-process-model limits.
//! Test: this *is* the test file.

use super::*;
use tokio::sync::Mutex as TokioMutex;

/// Process-wide lock that serialises every test in this module which
/// touches the `TRUSTY_BM25_EXTERNAL` or `TRUSTY_BM25_DAEMON_BIN` env
/// vars.
///
/// Why: cargo runs tests in parallel inside the same process, and any
/// two tests that mutate the same env var race each other. The lock
/// keeps the supervisor's env mutation isolated from sibling tests
/// without slowing the whole suite via `--test-threads=1`. We use
/// `tokio::sync::Mutex` here (not `std::sync::Mutex`) because the
/// guard is held across `.await` calls in `ensure_running`; holding a
/// std-sync guard across an await would block the runtime and is
/// flagged by `clippy::await_holding_lock`.
/// What: a static `OnceLock<Arc<TokioMutex<()>>>` initialised on first
/// access so we don't need a runtime to construct it. Each call
/// returns a clone of the `Arc` — cheap and lockable.
/// Test: used by every env-mutating test below.
fn env_lock() -> std::sync::Arc<TokioMutex<()>> {
    static LOCK: std::sync::OnceLock<std::sync::Arc<TokioMutex<()>>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Arc::new(TokioMutex::new(())))
        .clone()
}

/// Why: the supervisor must start with an empty map so the first
/// `ensure_running` call always takes the cold-path branch.
/// What: constructs a supervisor and asserts no children are tracked.
/// Test: this test itself.
#[tokio::test]
async fn supervisor_starts_empty() {
    let sup = Bm25Supervisor::new();
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: `Default::default()` must behave like `new()`. Catches a
/// regression where someone adds state to `new` and forgets to mirror
/// it on `Default`.
/// Test: this test itself.
#[tokio::test]
async fn supervisor_default_matches_new() {
    let sup: Bm25Supervisor = Default::default();
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: in external-management mode `ensure_running` must NOT spawn
/// anything; it must just hand back the socket path the caller would
/// have ended up at if it had spawned. Pinning this guards against a
/// future regression that accidentally fires off a child even with the
/// env var set.
/// What: set `TRUSTY_BM25_EXTERNAL=1`, call `ensure_running` against a
/// definitely-unused palace + a nonexistent data dir, assert no child
/// is tracked and the returned path matches the canonical resolver.
/// Test: this test itself. The env mutation is serialised by the
/// guard because Rust tests run in parallel inside the same process.
#[tokio::test]
async fn external_mode_skips_spawn() {
    let lock = env_lock();
    let _env = lock.lock().await;
    let _guard = EnvGuard::set(ENV_EXTERNAL_BM25, "1");
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = Bm25Supervisor::new();
    // Short palace name so the resolved socket path stays well under
    // the kernel's `sun_path` limit (~104 bytes on macOS).
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

/// Why: if some other process is already serving on the canonical
/// socket path (think: stale daemon from a previous run, or an
/// operator-managed launchd job that forgot the `TRUSTY_BM25_EXTERNAL`
/// env var), spawning a second daemon would EADDRINUSE. The supervisor
/// must adopt the existing socket and return without spawning.
/// What: bind a dummy `UnixListener` at the canonical socket path for
/// a test palace, call `ensure_running`, assert no child is tracked
/// and the returned path matches.
/// Test: this test itself.
#[tokio::test]
async fn already_running_skips_spawn() {
    let lock = env_lock();
    let _env = lock.lock().await;
    // Ensure we don't accidentally pick up an external-mode flag from
    // a sibling test that ran first.
    let _g = EnvGuard::remove(ENV_EXTERNAL_BM25);
    // Use a very short palace name. The canonical socket path is
    // `$TMPDIR/trusty-bm25-<palace>.sock`, and macOS' `$TMPDIR`
    // (`/var/folders/.../T/`) is already long, so we keep the palace
    // fragment to a handful of characters to avoid SUN_LEN errors.
    // Use the low bits of process PID to disambiguate concurrent
    // test runs.
    let palace = format!("a{:x}", std::process::id() & 0xffff);
    let socket = socket_path_for_palace(&palace);
    // Clean up any leftover socket from a previous failed test.
    let _ = std::fs::remove_file(&socket);
    let listener =
        tokio::net::UnixListener::bind(&socket).expect("bind dummy listener at canonical path");

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

/// Why: `shutdown` on a fresh supervisor must not panic, error, or
/// log anything alarming. Operators will inevitably call it at exit
/// even when no palace has touched BM25 yet.
/// Test: this test itself.
#[tokio::test]
async fn shutdown_with_no_children_is_noop() {
    let sup = Bm25Supervisor::new();
    sup.shutdown().await;
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: `Bm25Supervisor` is shared via `Arc` and must be `Send + Sync`
/// so it can be cloned into background tasks and async handlers.
/// Compile-fail of this test means the type bounds regressed.
/// What: a static assertion via a const fn that requires `Send + Sync`.
/// Test: this test itself.
#[test]
fn supervisor_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Bm25Supervisor>();
}

/// Why: the probe must report `false` for a path with nothing bound,
/// not panic or block forever.
/// Test: this test itself.
#[tokio::test]
async fn probe_returns_false_for_missing_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("nonexistent.sock");
    assert!(!probe_socket(&missing).await);
}

/// Why: the probe must report `true` immediately when something is
/// already accepting connections at that path. Pins the happy path.
/// Test: this test itself.
#[tokio::test]
async fn probe_returns_true_for_bound_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("listen.sock");
    let _listener = tokio::net::UnixListener::bind(&sock).expect("bind listener for probe test");
    assert!(probe_socket(&sock).await);
}

/// RAII guard for serialised env-var mutation in tests.
///
/// Why: cargo test runs tests in the same process by default, so
/// `std::env::set_var` mutations leak between tests unless restored
/// on drop. This is the same pattern the supervisor unit tests in
/// `trusty-search/src/service/embedder_supervisor.rs` use.
/// What: captures the prior value on construction, restores or
/// removes on drop. SAFETY notes inlined at each unsafe block.
/// Test: used by every env-touching test in this module.
struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: test-only env mutation; serialised by the fact that
        // each test takes the guard before mutating, and the Drop
        // impl restores on scope exit.
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
        // SAFETY: test teardown; restoring the captured prior value
        // is the inverse of the unsafe mutation done at construction.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}

// ── Bounded process model (#2845 / #2846) ─────────────────────────────────

/// Spawn a long-lived, cheap child to stand in for a BM25 daemon.
///
/// Why: the limit-enforcement paths care about how many children exist and how
/// much memory they use — not about what they serve. Driving them with real
/// `trusty-bm25-daemon` children would make these tests depend on a built
/// binary and a bound socket, which is what the `#[ignore]`d e2e test is for.
/// `sleep` is a real OS process with a real pid and real RSS, which is
/// everything the cap and the RSS reader need.
/// What: `sleep 60` with `kill_on_drop`, so a failing test cannot leak it.
/// Test: used by every limit test below.
fn stub_child() -> Child {
    Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn stub child")
}

/// Register a stub child under `palace` with an explicit LRU stamp.
///
/// Why: the LRU victim choice is the behaviour under test, so the test must
/// control the recency order directly rather than hoping wall-clock ordering
/// falls out of the call sequence.
/// What: inserts a `ChildHandle` into the supervisor's map. Reaches private
/// fields because this module is a child of `bm25_supervisor`.
/// Test: used by the cap tests below.
async fn register_stub(sup: &Bm25Supervisor, palace: &str, last_used: u64) {
    let tmp = std::env::temp_dir().join(format!("trusty-bm25-stub-{palace}.sock"));
    sup.children.lock().await.insert(
        palace.to_string(),
        ChildHandle {
            child: stub_child(),
            socket_path: tmp,
            last_used,
        },
    );
}

/// Why: the default has to be a small number, because the failure it prevents
/// is one `memory_recall_all` leaving ~99 resident subprocesses. A regression
/// that quietly widened it back out would restore that failure without
/// changing a single line of enforcement logic.
/// Test: this test itself.
#[test]
fn default_cap_is_three() {
    assert_eq!(DEFAULT_MAX_LIVE_DAEMONS, 3);
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

/// Why (#2846): `0` must be the documented, working way to turn RSS
/// enforcement off — distinct from the tightest possible ceiling. If those two
/// collapsed onto one value, an operator disabling the limit would instead get
/// a supervisor that reaps every daemon it spawns.
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

/// Why (#2845): this is the regression the cap exists for. Before it, the map
/// only ever grew — a fan-out across N palaces left N children resident for
/// the process lifetime. Against that code this assertion reads 4, not 2.
/// What: registers four stubs with increasing recency, asks for room for one
/// more with a cap of 3, and asserts the population lands at 2 (3 minus the
/// requested headroom) and that the two survivors are the most recently used.
/// Test: this test itself.
#[tokio::test]
async fn exceeding_the_cap_reaps_the_least_recently_used() {
    let sup = Bm25Supervisor::with_limits(3, None);
    for (i, palace) in ["oldest", "older", "newer", "newest"].iter().enumerate() {
        register_stub(&sup, palace, i as u64 + 1).await;
    }
    assert_eq!(sup.supervised_count().await, 4);

    sup.enforce_limits(1).await;

    assert_eq!(
        sup.supervised_count().await,
        2,
        "cap 3 with headroom 1 must leave room for the incoming daemon"
    );
    let live = sup.children.lock().await;
    assert!(live.contains_key("newest"), "most recent must survive");
    assert!(
        live.contains_key("newer"),
        "second most recent must survive"
    );
    assert!(!live.contains_key("oldest"), "LRU must be reaped first");
    drop(live);
    assert_eq!(
        sup.reaped_count(),
        2,
        "reaps must be counted, not just done"
    );
}

/// Why: a cap of 1 is the tightest legal setting and the one an operator on a
/// constrained host will pick. It must keep exactly one daemon, not zero —
/// reaping the daemon you are about to use is a wedge, not a limit.
/// Test: this test itself.
#[tokio::test]
async fn cap_of_one_keeps_a_single_daemon_live() {
    let sup = Bm25Supervisor::with_limits(1, None);
    register_stub(&sup, "a", 1).await;
    // Headroom 0: nothing incoming, so the single existing daemon stays.
    sup.enforce_limits(0).await;
    assert_eq!(sup.supervised_count().await, 1);
    // Headroom 1: an incoming daemon must displace the resident one.
    sup.enforce_limits(1).await;
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: a cap of 0 would mean "never keep a daemon", which makes every
/// `ensure_running` spawn-and-immediately-reap. Clamping to 1 turns an
/// operator's misconfiguration into a tight limit rather than a livelock.
/// Test: this test itself.
#[test]
fn cap_is_clamped_to_at_least_one() {
    assert_eq!(Bm25Supervisor::with_limits(0, None).max_live(), 1);
}

/// Why (#2846): this is the assertion trusty-search's `rss_limit_mb` never
/// had. A limit that is declared but never compared against a measurement
/// reaps nothing, which is indistinguishable from having no limit until the
/// kernel's OOM killer makes the distinction.
/// What: a ceiling of 0 MB means "at or above zero", which every live process
/// satisfies, so both stubs must be reaped on RSS grounds — and crucially NOT
/// on cap grounds, since the cap here is wide enough to hold them both.
/// Test: this test itself.
#[tokio::test]
async fn over_rss_limit_children_are_reaped() {
    let sup = Bm25Supervisor::with_limits(16, Some(0));
    register_stub(&sup, "fat-a", 1).await;
    register_stub(&sup, "fat-b", 2).await;
    assert_eq!(sup.supervised_count().await, 2);

    sup.enforce_limits(0).await;

    assert_eq!(
        sup.supervised_count().await,
        0,
        "children at or above the RSS ceiling must be reaped even under the cap"
    );
    assert_eq!(sup.reaped_count(), 2);
}

/// Why: a generous ceiling must leave healthy daemons alone. Without this the
/// previous test would also pass on an implementation that reaps
/// unconditionally, which is a different bug wearing the same result.
/// Test: this test itself.
#[tokio::test]
async fn under_rss_limit_children_are_left_alone() {
    let sup = Bm25Supervisor::with_limits(16, Some(1_000_000));
    register_stub(&sup, "lean", 1).await;
    sup.enforce_limits(0).await;
    assert_eq!(sup.supervised_count().await, 1);
    assert_eq!(sup.reaped_count(), 0);
}

/// Why (#2846 inverse): `None` from the RSS reader means "no reading", not
/// "zero". Treating an unavailable measurement as a breach would reap every
/// healthy daemon on any platform where the read is unsupported — converting a
/// memory guardrail into an outage.
/// What: pid 0 is not measurable on either supported platform, and `None`
/// stands for a child that has already exited.
/// Test: this test itself.
#[test]
fn over_rss_limit_is_false_without_a_measurement() {
    assert!(
        !over_rss_limit(Some(0), Some(0)),
        "unmeasurable must not reap"
    );
    assert!(!over_rss_limit(None, Some(0)), "exited child must not reap");
}

/// Why: disabling enforcement must actually disable it, including for a
/// process that would otherwise be over any ceiling.
/// Test: this test itself.
#[test]
fn over_rss_limit_is_false_when_disabled() {
    assert!(!over_rss_limit(Some(std::process::id()), None));
}

/// Why: `shutdown` reaps for a different reason than the limits do, and an
/// operator reading `reaped_count` to answer "is my cap doing anything?" must
/// not have normal shutdown inflate the number.
/// Test: this test itself.
#[tokio::test]
async fn shutdown_does_not_count_as_a_limit_reap() {
    let sup = Bm25Supervisor::with_limits(16, None);
    register_stub(&sup, "bye", 1).await;
    sup.shutdown().await;
    assert_eq!(sup.supervised_count().await, 0);
    assert_eq!(sup.reaped_count(), 0);
}

/// Why (#5048 review): the supervisor's SIGTERM patience must exceed the
/// daemon's own shutdown-flush budget with margin. At an equal budget the
/// SIGKILL lands inside the flush the durability fix added and the open write
/// window is lost. Pinning the relationship here means a future tightening of
/// either number has to be a deliberate choice.
/// What: asserts the patience is strictly greater than
/// `trusty_bm25_daemon`'s 2 s `SHUTDOWN_FLUSH_TIMEOUT`, with real margin.
/// Test: this test itself.
#[test]
fn sigterm_patience_exceeds_the_daemon_flush_budget() {
    // The daemon's own budget. Not importable — `trusty-memory` depends on the
    // daemon crate only for the bundled binary shim, and the constant is
    // private — so it is restated with a pointer, the same way the BM25 client
    // restates the wire method names.
    let daemon_flush_budget = Duration::from_secs(2);
    assert!(
        SIGTERM_PATIENCE > daemon_flush_budget,
        "the supervisor must outwait the daemon's flush: {SIGTERM_PATIENCE:?} vs \
         {daemon_flush_budget:?}"
    );
    assert!(
        SIGTERM_PATIENCE - daemon_flush_budget >= Duration::from_secs(2),
        "leave room for signal delivery, socket cleanup and process exit on top \
         of the flush itself"
    );
}
