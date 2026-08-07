//! Unit tests for [`super::UdsServiceSupervisor`] (#5089 step 2).
//!
//! Why: the limits, the probe classification and the eviction bookkeeping moved
//! here from `trusty-memory`'s `bm25_supervisor_tests.rs` along with the code
//! they cover — the machinery is no longer BM25's, so its unit coverage cannot
//! stay behind. Every assertion in those tests has a counterpart below, plus the
//! cases the generalisation newly makes reachable: a per-service timeout
//! relationship, an untrusted adoption, and a spawn budget that expires.
//! What: stub children are real `sleep` processes — a real pid with real RSS is
//! everything the cap and the RSS reader need, and it keeps these tests free of
//! any built daemon binary.
//!
//! Every test is `#[serial]`, and that is load-bearing rather than tidy.
//! Measured on this file: `probe_verdict_reports_not_serving_for_a_stale_socket_file`
//! passed 3/3 alone and 3/3 alongside the other probe tests, but failed 4 of 5
//! runs of the whole module and 2 of 3 runs of the wider `uds::` filter —
//! reading `Serving` for a listener this test had already dropped.
//!
//! The mechanism is **fd inheritance across a concurrent fork**. macOS has no
//! atomic `SOCK_CLOEXEC`, so `UnixListener::bind` creates the socket and then
//! sets `FD_CLOEXEC` in a second syscall. A `Command::spawn` on another test
//! thread that forks inside that window hands the listening fd to the child —
//! here a `sleep 60` stub — and the child holds it open. The parent's
//! `drop(listener)` then closes only its own descriptor, the socket stays bound,
//! and the connect this test expects to be refused completes instead. It is a
//! property of the test harness, not of the supervisor: a supervised service
//! binds its own socket in its own process, and this supervisor never holds one
//! to leak. Serialising the fork-heavy tests against the liveness assertions
//! closes the window. `trusty-memory`'s `bm25_supervisor_concurrency.rs` reached
//! the same remedy for the same assertion. It removes no coverage.
//! Test: this *is* the test file.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use super::child::{ChildHandle, terminate_child};
use super::*;

/// Timeouts for a service that binds instantly and flushes nothing.
///
/// Deliberately tight so a test that waits out the spawn budget costs
/// milliseconds rather than seconds.
const TEST_TIMEOUTS: ServiceTimeouts = ServiceTimeouts::new(
    Duration::from_millis(400),
    Duration::from_millis(50),
    Duration::from_secs(5),
);

fn config(max_live: usize, rss_limit_mb: Option<u64>) -> SupervisorConfig {
    SupervisorConfig::new("test-service", max_live, TEST_TIMEOUTS).with_rss_limit_mb(rss_limit_mb)
}

fn supervisor(max_live: usize, rss_limit_mb: Option<u64>) -> UdsServiceSupervisor {
    UdsServiceSupervisor::new(config(max_live, rss_limit_mb))
}

/// A long-lived, cheap child standing in for a supervised service.
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

/// Register a stub child under `key` with an explicit LRU stamp.
///
/// Why: the LRU victim choice is the behaviour under test, so the test must
/// control the recency order directly rather than hoping wall-clock ordering
/// falls out of the call sequence.
async fn register_stub(sup: &UdsServiceSupervisor, key: &str, socket: PathBuf, last_used: u64) {
    sup.children.lock().await.insert(
        key.to_string(),
        ChildHandle {
            child: stub_child(),
            socket_path: socket,
            last_used,
        },
    );
}

/// A spec closure that must never be called.
fn never_spawn() -> Result<SpawnSpec, Box<dyn std::error::Error + Send + Sync + 'static>> {
    panic!("the supervisor must not resolve a spawn spec on this path");
}

/// RAII guard for serialised env-var mutation in tests.
struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: test-only env mutation, serialised by `#[serial]` on every
        // test that constructs one; the Drop impl restores on scope exit.
        unsafe { std::env::set_var(key, value) }
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

// ── ServiceTimeouts: the promoted compile-time invariant ──────────────────

/// Why (#5085 / #5089): this is what replaces `bm25_supervisor.rs`'s
/// `const _: () = assert!(SIGTERM_PATIENCE_SECS > …SHUTDOWN_FLUSH_TIMEOUT…)`.
/// A patience equal to the child's flush budget lets the SIGKILL land inside the
/// very flush the child's shutdown handler exists to perform. The `const fn`
/// makes the check a compile error for a service that declares its timeouts as a
/// `const`; this pins the runtime half of the same guard.
/// Test: this test itself.
#[serial_test::serial]
#[test]
#[should_panic(expected = "SIGTERM patience must strictly exceed")]
fn service_timeouts_reject_patience_equal_to_the_flush() {
    let _ = ServiceTimeouts::new(
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(2),
    );
}

/// Why: an inverted pair is the same defect as an equal one, and the comparison
/// is hand-written (`Duration`'s own operators are not `const`), so both sides
/// of it need a test.
/// Test: this test itself.
#[serial_test::serial]
#[test]
#[should_panic(expected = "SIGTERM patience must strictly exceed")]
fn service_timeouts_reject_patience_below_the_flush() {
    let _ = ServiceTimeouts::new(
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(1),
    );
}

/// Why: the hand-written const comparison must get sub-second margins right —
/// a service with a 500 ms flush and a 600 ms patience is legal, and an
/// implementation that only compared whole seconds would reject it.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn service_timeouts_accept_a_subsecond_margin() {
    let t = ServiceTimeouts::new(
        Duration::from_millis(100),
        Duration::from_millis(500),
        Duration::from_millis(600),
    );
    assert_eq!(t.sigterm_patience, Duration::from_millis(600));
}

/// Why: `new` is a `const fn`, so the only thing it can do about a bad pair at
/// runtime is panic — and `#[non_exhaustive]` leaves no other way to build the
/// value. A service deriving its timeouts from config needs to report that as an
/// error rather than take the process down.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn try_new_rejects_an_inverted_pair_without_panicking() {
    let err = ServiceTimeouts::try_new(
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .expect_err("an equal pair must be rejected");
    assert!(
        matches!(err, SupervisorError::InvalidTimeouts { .. }),
        "expected InvalidTimeouts, got {err:?}"
    );
}

/// Why: the fallible constructor must be the same check, not a second one that
/// could drift from the `const fn`'s.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn try_new_matches_new_for_a_valid_pair() {
    let built = ServiceTimeouts::try_new(
        Duration::from_millis(400),
        Duration::from_millis(50),
        Duration::from_secs(5),
    )
    .expect("a valid pair must be accepted");
    assert_eq!(built, TEST_TIMEOUTS);
}

/// Why: the probe cadence is supervision's own concern, not the service's, so
/// `new` must supply working defaults and the overrides must actually take.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn service_timeouts_carry_probe_defaults_and_honour_overrides() {
    assert_eq!(
        TEST_TIMEOUTS.initial_probe_interval,
        DEFAULT_INITIAL_PROBE_INTERVAL
    );
    assert_eq!(TEST_TIMEOUTS.max_probe_interval, DEFAULT_MAX_PROBE_INTERVAL);
    assert_eq!(TEST_TIMEOUTS.connect_probe, DEFAULT_CONNECT_PROBE_TIMEOUT);

    let tuned = TEST_TIMEOUTS
        .with_probe_intervals(Duration::from_millis(1), Duration::from_millis(2))
        .with_connect_probe(Duration::from_millis(3));
    assert_eq!(tuned.initial_probe_interval, Duration::from_millis(1));
    assert_eq!(tuned.max_probe_interval, Duration::from_millis(2));
    assert_eq!(tuned.connect_probe, Duration::from_millis(3));
    assert_eq!(
        tuned.spawn_probe, TEST_TIMEOUTS.spawn_probe,
        "an override must not disturb the service's own budget"
    );
}

/// Why: a cap of 0 would mean "never keep a child", making every
/// `ensure_running` spawn-and-immediately-reap. Clamping turns an operator's
/// misconfiguration into a tight limit rather than a livelock.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn supervisor_config_clamps_max_live_to_one() {
    assert_eq!(supervisor(0, None).max_live(), 1);
}

/// Why: the opt-out exists so an operator running the target under `tctl`
/// (ADR-0011 / ADR-0034 §1) keeps its lifecycle. A loose truthiness check would
/// let `TRUSTY_X_EXTERNAL=0` disable supervision, which is the opposite of what
/// the operator wrote.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn external_env_only_honours_exactly_one() {
    const VAR: &str = "TRUSTY_TEST_SUPERVISOR_EXTERNAL";
    let cfg = config(2, None).with_external_env(VAR);

    let _g = EnvGuard::set(VAR, "1");
    assert!(cfg.external_mode_enabled());
    let _g = EnvGuard::set(VAR, "0");
    assert!(!cfg.external_mode_enabled());
    let _g = EnvGuard::set(VAR, "true");
    assert!(!cfg.external_mode_enabled());

    assert!(
        !config(2, None).external_mode_enabled(),
        "a config with no external var can never be opted out"
    );
}

/// Why: the builder is the only way to construct a spec from outside this
/// crate, so silent argument loss would be undetectable at the call site.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn spawn_spec_builder_accumulates_args_and_dirs() {
    let spec = SpawnSpec::new("/bin/echo")
        .arg("--palace")
        .arg("alpha")
        .create_dir("/tmp/a")
        .create_dir("/tmp/b");
    assert_eq!(spec.program, PathBuf::from("/bin/echo"));
    assert_eq!(spec.args, vec!["--palace", "alpha"]);
    assert_eq!(spec.create_dirs.len(), 2);
}

// ── Socket-backed liveness (#5085) ────────────────────────────────────────

/// Why (#5085): eviction keys off `NotServing` specifically, so an absent socket
/// must produce that verdict and not the `Inconclusive` catch-all — otherwise a
/// crashed child is never replaced.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn probe_verdict_reports_not_serving_for_a_missing_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("nonexistent.sock");
    assert_eq!(
        probe_socket_verdict(&missing, DEFAULT_CONNECT_PROBE_TIMEOUT).await,
        SocketVerdict::NotServing
    );
    assert!(!socket_is_serving(&missing, DEFAULT_CONNECT_PROBE_TIMEOUT).await);
}

/// Why (#5085): a stale socket file left behind by a SIGKILLed child answers
/// ECONNREFUSED rather than ENOENT. That is still proof nothing is listening,
/// and it is the shape a real crash leaves on disk.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn probe_verdict_reports_not_serving_for_a_stale_socket_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("stale.sock");
    let listener = tokio::net::UnixListener::bind(&sock).expect("bind listener");
    drop(listener);
    assert!(sock.exists(), "the socket file must outlive the listener");
    assert_eq!(
        probe_socket_verdict(&sock, DEFAULT_CONNECT_PROBE_TIMEOUT).await,
        SocketVerdict::NotServing
    );
}

/// Why (#5085): a serving socket must never be classified as `NotServing`, or
/// `lookup_live` would evict healthy children on every call.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn probe_verdict_reports_serving_for_a_bound_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("listen.sock");
    let _listener = tokio::net::UnixListener::bind(&sock).expect("bind listener");
    assert_eq!(
        probe_socket_verdict(&sock, DEFAULT_CONNECT_PROBE_TIMEOUT).await,
        SocketVerdict::Serving
    );
    assert!(socket_is_serving(&sock, DEFAULT_CONNECT_PROBE_TIMEOUT).await);
}

/// Why: the backoff loop must terminate on the service's own budget, not on a
/// constant. A loop that ignored `spawn_probe` would hang a caller for whatever
/// the old BM25 3 s value happened to be.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn wait_for_socket_gives_up_within_the_spawn_budget() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("never.sock");
    let budget = Duration::from_millis(120);
    let timeouts = ServiceTimeouts::new(budget, Duration::from_millis(10), Duration::from_secs(1));

    let started = std::time::Instant::now();
    assert!(!super::probe::wait_for_socket(&missing, &timeouts).await);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= budget,
        "must not give up before the budget: {elapsed:?}"
    );
    assert!(
        elapsed < budget * 8,
        "must not overshoot the budget by an order of magnitude: {elapsed:?}"
    );
}

/// Why: the affirmative half — a socket that IS bound must be detected on the
/// first probe rather than after a full backoff.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn wait_for_socket_returns_once_the_socket_binds() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("bound.sock");
    let _listener = tokio::net::UnixListener::bind(&sock).expect("bind listener");
    assert!(super::probe::wait_for_socket(&sock, &TEST_TIMEOUTS).await);
}

// ── Child lifecycle ───────────────────────────────────────────────────────

/// Why (#2846 inverse): `None` from the RSS reader means "no reading", not
/// "zero". Treating an unavailable measurement as a breach would reap every
/// healthy child on any platform where the read is unsupported — converting a
/// memory guardrail into an outage.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn over_rss_limit_is_false_without_a_measurement() {
    assert!(
        !over_rss_limit(Some(0), Some(0)),
        "unmeasurable must not reap"
    );
    assert!(!over_rss_limit(None, Some(0)), "exited child must not reap");
}

/// Why: disabling enforcement must actually disable it, including for a process
/// that would otherwise be over any ceiling.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn over_rss_limit_is_false_when_disabled() {
    assert!(!over_rss_limit(Some(std::process::id()), None));
}

/// Why: SIGTERM is the only signal a child's own shutdown handler can act on, so
/// "terminate" that quietly SIGKILLed would defeat every flush this supervisor
/// exists to allow.
/// What: `sleep` dies on SIGTERM well inside the patience window, so a
/// terminate that returns without escalating is the observable proof.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn terminate_child_reaps_a_live_child() {
    let mut child = stub_child();
    terminate_child(&mut child, Duration::from_secs(5))
        .await
        .expect("terminate a live child");
    assert!(
        child.try_wait().expect("try_wait").is_some(),
        "the child must be reaped by the time terminate returns"
    );
}

/// Why: a spec that names directories expects them to exist before the child
/// runs — a service told to write into a missing data dir would otherwise exit
/// immediately and be reported as a spawn timeout.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn spawn_child_creates_requested_directories() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("nested/data");
    let spec = SpawnSpec::new("/bin/echo").create_dir(&data);
    let mut child = super::child::spawn_child("test-service", "k", &spec)
        .await
        .expect("spawn");
    let _ = child.wait().await;
    assert!(data.is_dir(), "the spec's directory must exist after spawn");
}

/// Why: a missing binary must surface as a spawn error naming the program, not
/// as a spawn timeout naming the socket — the two send an operator to different
/// places.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn spawn_child_reports_a_missing_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spec = SpawnSpec::new(tmp.path().join("no-such-binary"));
    let err = super::child::spawn_child("test-service", "k", &spec)
        .await
        .expect_err("a missing binary must fail");
    assert!(
        matches!(err, SupervisorError::Spawn { .. }),
        "expected Spawn, got {err:?}"
    );
}

// ── Supervisor state machine ──────────────────────────────────────────────

/// Why: the supervisor must start with an empty map so the first
/// `ensure_running` always takes the cold path.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn supervisor_starts_empty() {
    let sup = supervisor(3, None);
    assert_eq!(sup.supervised_count().await, 0);
    assert_eq!(sup.spawned_count(), 0);
    assert_eq!(sup.reaped_count(), 0);
}

/// Why: the supervisor is shared via `Arc` across handlers; a regression in its
/// bounds would only show up at a distant call site.
/// Test: this test itself.
#[serial_test::serial]
#[test]
fn supervisor_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<UdsServiceSupervisor>();
}

/// Why: `shutdown` on a fresh supervisor must not panic or error. Callers will
/// invoke it at exit even when nothing was ever spawned.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn shutdown_with_no_children_is_noop() {
    let sup = supervisor(3, None);
    sup.shutdown().await;
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why: in external-management mode `ensure_running` must not spawn anything or
/// even try to resolve a spawn spec — the operator owns that lifecycle, and a
/// supervisor that located the binary anyway would fail on hosts where it is not
/// installed at all.
/// What: the spec closure panics if called, which is the strongest available
/// assertion that this path never reaches it.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn external_mode_skips_spawn() {
    const VAR: &str = "TRUSTY_TEST_SUPERVISOR_EXTERNAL";
    let _g = EnvGuard::set(VAR, "1");
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("external.sock");
    let sup = UdsServiceSupervisor::new(config(3, None).with_external_env(VAR));

    let path = sup
        .ensure_running("inst", &socket, never_spawn)
        .await
        .expect("external mode must return the socket path without spawning");
    assert_eq!(path, socket);
    assert_eq!(sup.supervised_count().await, 0);
    assert_eq!(sup.spawned_count(), 0);
}

/// Why: the fast path is the common case, and it must be reached without
/// resolving a spawn spec. It is also the path #5085 made socket-backed: a
/// stored child counts as live only when its socket answers.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn a_serving_child_is_reused_without_a_spawn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("live.sock");
    let _listener = tokio::net::UnixListener::bind(&socket).expect("bind");
    let sup = supervisor(3, None);
    register_stub(&sup, "inst", socket.clone(), 1).await;

    let path = sup
        .ensure_running("inst", &socket, never_spawn)
        .await
        .expect("a serving child must be reused");
    assert_eq!(path, socket);
    assert_eq!(sup.spawned_count(), 0);
    assert_eq!(sup.supervised_count().await, 1);
    sup.shutdown().await;
}

/// Why: a child that has exited must be removed so the caller falls through to a
/// fresh spawn, and removing a corpse reclaims nothing — counting it as a reap
/// would make `reaped_count` useless for answering "is my cap doing anything?".
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn a_dead_child_is_evicted_without_counting_as_a_reap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("dead.sock");
    let sup = supervisor(3, None);
    {
        let mut child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn a child that exits immediately");
        child.wait().await.expect("wait");
        sup.children.lock().await.insert(
            "inst".to_string(),
            ChildHandle {
                child,
                socket_path: socket.clone(),
                last_used: 1,
            },
        );
    }

    assert!(sup.lookup_live("inst").await.is_none());
    assert_eq!(sup.supervised_count().await, 0);
    assert_eq!(sup.reaped_count(), 0);
    assert_eq!(
        sup.doomed.lock().await.len(),
        0,
        "a corpse owes no flush and must not enter the doomed queue"
    );
}

/// Why (#5085 / #5119): the unserved-socket arm is the only eviction that
/// removes a LIVE child, and a live child may hold acked work only its own
/// SIGTERM handler writes. It must therefore be queued for graceful termination
/// rather than dropped onto `kill_on_drop`'s SIGKILL — and drained under the
/// spawn gate, before anything rebinds the path it is about to unlink.
/// What: a live stub whose socket does not exist. `try_wait()` reports it alive
/// forever, so nothing here depends on timing.
/// Test: this test itself. Replace the doomed queue with a bare drop and the
/// queue length reads 0.
#[serial_test::serial]
#[tokio::test]
async fn an_unserved_live_child_is_queued_for_graceful_termination() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("gone.sock");
    let sup = supervisor(3, None);
    register_stub(&sup, "inst", socket, 1).await;

    assert!(sup.lookup_live("inst").await.is_none());
    assert_eq!(sup.supervised_count().await, 0);
    assert_eq!(
        sup.doomed.lock().await.len(),
        1,
        "a live evictee owes a flush and must be queued, not dropped"
    );

    sup.reap_doomed().await;
    assert_eq!(sup.doomed.lock().await.len(), 0);
    assert_eq!(
        sup.reaped_count(),
        0,
        "a restart is not a reclamation and must not inflate the reap count"
    );
}

/// Why (#2845): this is the regression the cap exists for. Before it the map
/// only ever grew — a fan-out across N keys left N children resident for the
/// process lifetime.
/// What: four stubs with increasing recency, room asked for one more against a
/// cap of 3. The population must land at 2 and the survivors must be the two
/// most recently used.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn exceeding_the_cap_reaps_the_least_recently_used() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = supervisor(3, None);
    for (i, key) in ["oldest", "older", "newer", "newest"].iter().enumerate() {
        register_stub(&sup, key, tmp.path().join(key), i as u64 + 1).await;
    }
    assert_eq!(sup.supervised_count().await, 4);

    sup.enforce_limits(1).await;

    assert_eq!(
        sup.supervised_count().await,
        2,
        "cap 3 with headroom 1 must leave room for the incoming child"
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
/// constrained host picks. It must keep exactly one child, not zero.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn cap_of_one_keeps_a_single_child_live() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = supervisor(1, None);
    register_stub(&sup, "a", tmp.path().join("a"), 1).await;
    sup.enforce_limits(0).await;
    assert_eq!(sup.supervised_count().await, 1);
    sup.enforce_limits(1).await;
    assert_eq!(sup.supervised_count().await, 0);
}

/// Why (#2846): this is the assertion trusty-search's `rss_limit_mb` never had.
/// A limit that is declared but never compared against a measurement reaps
/// nothing, which is indistinguishable from having no limit until the kernel's
/// OOM killer makes the distinction.
/// What: a ceiling of 0 MB means "at or above zero", which every live process
/// satisfies — so both stubs must be reaped on RSS grounds, and crucially NOT on
/// cap grounds, since the cap here holds them both.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn over_rss_limit_children_are_reaped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = supervisor(16, Some(0));
    register_stub(&sup, "fat-a", tmp.path().join("a"), 1).await;
    register_stub(&sup, "fat-b", tmp.path().join("b"), 2).await;
    assert_eq!(sup.supervised_count().await, 2);

    sup.enforce_limits(0).await;

    assert_eq!(
        sup.supervised_count().await,
        0,
        "children at or above the RSS ceiling must be reaped even under the cap"
    );
    assert_eq!(sup.reaped_count(), 2);
}

/// Why: a generous ceiling must leave healthy children alone. Without this the
/// previous test would also pass against an implementation that reaps
/// unconditionally — a different bug wearing the same result.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn under_rss_limit_children_are_left_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = supervisor(16, Some(1_000_000));
    register_stub(&sup, "lean", tmp.path().join("lean"), 1).await;
    sup.enforce_limits(0).await;
    assert_eq!(sup.supervised_count().await, 1);
    assert_eq!(sup.reaped_count(), 0);
}

/// Why: `shutdown` reaps for a different reason than the limits do, and an
/// operator reading `reaped_count` to answer "is my cap doing anything?" must
/// not have normal shutdown inflate the number.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn shutdown_does_not_count_as_a_limit_reap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sup = supervisor(16, None);
    register_stub(&sup, "bye", tmp.path().join("bye"), 1).await;
    sup.shutdown().await;
    assert_eq!(sup.supervised_count().await, 0);
    assert_eq!(sup.reaped_count(), 0);
}

/// Why (ADR-0034 §3): adoption is the one path where the service's own
/// `bind_hardened` never runs, so without a check here an unhardened — or
/// foreign — socket would be adopted silently, and the permission bits the relay
/// hop rests on would be a documented intention rather than a boundary. #5099's
/// `connect_hardened` doc names this exact path as the reason a dialer must
/// verify.
/// What: a socket that IS serving but sits at mode 0666 must be refused with
/// `UntrustedSocket` rather than adopted, and rather than falling through to a
/// spawn that dies on EADDRINUSE.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn adoption_refuses_a_world_writable_socket() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("wide.sock");
    let _listener = tokio::net::UnixListener::bind(&socket).expect("bind");
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))
        .expect("widen the socket");

    let sup = supervisor(3, None);
    let err = sup
        .ensure_running("inst", &socket, never_spawn)
        .await
        .expect_err("an unhardened socket must not be adopted");
    assert!(
        matches!(err, SupervisorError::UntrustedSocket { .. }),
        "expected UntrustedSocket, got {err:?}"
    );
    assert_eq!(sup.spawned_count(), 0);
}

/// Why: the affirmative half of adoption. A socket some other process bound
/// through `bind_hardened` — an operator-managed job that forgot the external
/// opt-out, or a child this supervisor lost the handle to — must be adopted, or
/// the spawn that follows would EADDRINUSE.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn adoption_accepts_a_hardened_socket_without_spawning() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("hardened.sock");
    let _listener = crate::uds::bind_hardened(&socket).expect("bind_hardened");

    let sup = supervisor(3, None);
    let path = sup
        .ensure_running("inst", &socket, never_spawn)
        .await
        .expect("a hardened serving socket must be adopted");
    assert_eq!(path, socket);
    assert_eq!(sup.spawned_count(), 0);
    assert_eq!(
        sup.supervised_count().await,
        0,
        "adoption must not register a child this supervisor does not own"
    );
}

/// Why (#5089): the spawn budget is now the SERVICE's number, and the error has
/// to say so — a 3 s budget carried over from a service with nothing to load
/// produces a timeout that looks like a broken binary rather than a mistuned
/// constant.
/// What: a child that runs but never binds, against a deliberately tiny budget.
/// The error must name that budget and the child must still have been launched.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn a_child_that_never_binds_fails_with_the_service_budget() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("never.sock");
    let budget = Duration::from_millis(150);
    let cfg = SupervisorConfig::new(
        "test-service",
        3,
        ServiceTimeouts::new(budget, Duration::from_millis(10), Duration::from_secs(1)),
    );
    let sup = UdsServiceSupervisor::new(cfg);

    let err = sup
        .ensure_running("inst", &socket, || Ok(SpawnSpec::new("sleep").arg("30")))
        .await
        .expect_err("a child that never binds must not be reported as running");
    match err {
        SupervisorError::SpawnTimeout {
            budget: reported, ..
        } => assert_eq!(
            reported, budget,
            "the error must carry the service's budget"
        ),
        other => panic!("expected SpawnTimeout, got {other:?}"),
    }
    assert_eq!(sup.spawned_count(), 1, "the launch itself did happen");
    assert_eq!(
        sup.supervised_count().await,
        0,
        "a child that never bound must not be registered"
    );
}

/// Why: a spec closure that fails must surface as its own variant rather than as
/// a spawn or timeout error — "the binary is missing" and "it bound too slowly"
/// send an operator to different places.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn a_failing_spawn_spec_is_reported_as_such() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("nospec.sock");
    let sup = supervisor(3, None);

    let err = sup
        .ensure_running("inst", &socket, || {
            Err("no binary on PATH".to_string().into())
        })
        .await
        .expect_err("an unresolvable spec must fail");
    assert!(
        matches!(err, SupervisorError::SpawnSpec { .. }),
        "expected SpawnSpec, got {err:?}"
    );
    assert_eq!(sup.spawned_count(), 0);
}

/// Why: an over-long socket path can never be bound, and Rust rejects it with a
/// bare `invalid argument` that names neither the limit nor the path. Catching it
/// before the spawn turns "the daemon is broken" into "this name is N bytes too
/// long" — and avoids launching a child that is guaranteed to fail.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn an_over_long_socket_path_is_rejected_before_spawning() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("x".repeat(crate::uds::sun_path_capacity()));
    let sup = supervisor(3, None);

    let err = sup
        .ensure_running("inst", &socket, never_spawn)
        .await
        .expect_err("an unbindable path must fail before the spawn");
    assert!(
        matches!(err, SupervisorError::SocketPath { .. }),
        "expected SocketPath, got {err:?}"
    );
    assert_eq!(sup.spawned_count(), 0);
}
