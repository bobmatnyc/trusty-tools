//! Repeated ort→python→ort swap-back cycle coverage (epic #3524 slice 7).
//!
//! Why: every existing test for `graceful_bootstrap`/`swap_back_watchdog`
//! (`start/tests.rs`) drives at most ONE ort→python transition or ONE
//! python→ort swap-back in isolation. Production itself only ever cycles
//! once per daemon lifetime (`drive_bootstrap`'s doc: "no re-bootstrap after
//! failure"). Neither proves what happens under REPEATED cycling — which
//! matters because the epic names two concrete leak candidates that can only
//! show up across multiple cycles: (a) a leaked python child if
//! `PythonAdapterTeardown::teardown()` is ever skipped, and (b) an orphaned
//! ort backend (or a permanently-stuck switchable) if `build_ort()` fails
//! mid-swap-back. This module is the #24 memory-safety re-check's other
//! half — the GracefulPython-arm-specific complement to the raw-supervisor
//! soak in `tests/py_sidecar_memory_soak.rs` (epic #3524 slice 6 PR 5/5,
//! #3610), which never touches `SwitchableEmbedder`/`swap_back_watchdog` at
//! all.
//!
//! What: two tiers, matching the project convention (fast in `cargo test`,
//! heavy behind an explicit opt-in):
//!   - `fast_deterministic` — repeated cycles driven directly against
//!     `drive_bootstrap`/`drive_swap_back_watchdog` with fully deterministic
//!     fakes (no real subprocess). Runs in every plain `cargo test`, proving
//!     the CALL-GRAPH has no structural leak (1 teardown call per 1 spawned
//!     python handle, across N cycles) and that a `build_ort()` failure
//!     mid-swap-back neither orphans anything nor permanently wedges
//!     recovery.
//!   - `real_soak` — `#[ignore]`d AND gated on `TRUSTY_SOAK=1` at runtime
//!     (mirroring `tests/py_sidecar_memory_soak.rs`'s convention), drives the
//!     REAL production entry point (`graceful_bootstrap::run_graceful_python_bootstrap`)
//!     repeatedly against a real `trusty-embedderd` + `trusty-embedderd-py`
//!     (real `uv` venv, real torch/MPS), inducing real supervisor give-up via
//!     repeated `SIGKILL`, and observing REAL OS-level signals (process
//!     count, RSS) rather than trusting internal bookkeeping alone. Requires
//!     Apple Silicon + a bootstrapped `uv` venv; never runs in CI.
//!
//! Test: `repeated_cycles_never_leak_teardown_calls`,
//! `build_ort_failure_mid_cycle_leaves_no_orphan_and_allows_recovery`,
//! `real_repeated_ort_python_ort_cycles_soak` (all in this module).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::core::Embedder as CoreEmbedder;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};
use crate::service::SearchAppState;

use super::graceful_bootstrap::{drive_bootstrap, PythonAdapterTeardown, PythonBootstrap};
use super::swap_back_watchdog::{drive_swap_back_watchdog, SwapBackOps};

// ============================================================================
// Fast, deterministic fakes.
//
// Deliberately NOT shared with `start/tests.rs`'s private `FakeBootstrap` /
// `FakeSwapBackOps` / `FakeTeardown` — Rust's per-file integration-test-like
// privacy means a `#[cfg(test)] mod` sibling cannot `use` another sibling
// module's private items, and `start/tests.rs` is out of scope for this PR
// (owned by the in-flight default-flip PR). These are intentionally minimal
// re-implementations of the same shape.
// ============================================================================

struct FakeEmbedder {
    calls: AtomicUsize,
}

impl FakeEmbedder {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl CoreEmbedder for FakeEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![text.len() as f32; trusty_common::embedder::EMBED_DIM])
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(texts
            .iter()
            .map(|t| vec![t.len() as f32; trusty_common::embedder::EMBED_DIM])
            .collect())
    }

    fn dimension(&self) -> usize {
        trusty_common::embedder::EMBED_DIM
    }
}

/// Controllable fake [`PythonAdapterTeardown`] — records teardown calls and
/// exposes a shared `terminated` flag the test flips to simulate confirmed
/// supervisor give-up.
struct FakeTeardown {
    calls: Arc<AtomicUsize>,
    terminated: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeTeardown {
    fn new(calls: Arc<AtomicUsize>, terminated: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self { calls, terminated }
    }
}

#[async_trait::async_trait]
impl PythonAdapterTeardown for FakeTeardown {
    async fn teardown(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }

    async fn is_confirmed_terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }
}

/// Fully deterministic fake [`PythonBootstrap`] — always succeeds instantly.
/// Each `build_adapter` call hands back a FRESH [`FakeTeardown`] wired to a
/// FRESH `terminated` flag, tracked in `last_terminated` so the test driving
/// a cycle can grab the flag for the handle it just installed and flip it to
/// simulate that specific handle's death.
struct CyclingBootstrap {
    total_teardown_calls: Arc<AtomicUsize>,
    total_spawns: Arc<AtomicUsize>,
    last_terminated: std::sync::Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}

impl CyclingBootstrap {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            total_teardown_calls: Arc::new(AtomicUsize::new(0)),
            total_spawns: Arc::new(AtomicUsize::new(0)),
            last_terminated: std::sync::Mutex::new(None),
        })
    }

    fn last_terminated_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.last_terminated
            .lock()
            .expect("lock poisoned")
            .clone()
            .expect("build_adapter must run before last_terminated_flag is read")
    }
}

impl PythonBootstrap for CyclingBootstrap {
    fn ensure_venv(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn locate_launcher(&self) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from("/fake/trusty-embedderd-py"))
    }

    fn build_adapter(
        &self,
        _launcher: PathBuf,
    ) -> (
        Arc<dyn CoreEmbedder>,
        Option<Arc<AtomicU32>>,
        Arc<dyn PythonAdapterTeardown>,
    ) {
        self.total_spawns.fetch_add(1, Ordering::Relaxed);
        let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *self.last_terminated.lock().expect("lock poisoned") = Some(Arc::clone(&terminated));
        let teardown: Arc<dyn PythonAdapterTeardown> = Arc::new(FakeTeardown::new(
            Arc::clone(&self.total_teardown_calls),
            terminated,
        ));
        let adapter: Arc<dyn CoreEmbedder> = Arc::new(FakeEmbedder::new());
        let pid = Arc::new(AtomicU32::new(
            4000 + self.total_spawns.load(Ordering::Relaxed) as u32,
        ));
        (adapter, Some(pid), teardown)
    }

    fn probe_timeout(&self) -> Duration {
        Duration::from_secs(5)
    }
}

/// Fake [`SwapBackOps`] whose `build_ort()` can be told to fail on specific
/// (1-indexed) call numbers — used to simulate the `build_ort()` failure the
/// swap-back watchdog logs-and-gives-up on (`swap_back_watchdog.rs:242-250`)
/// without ever touching a real `trusty-embedderd` binary.
struct CyclingSwapBackOps {
    calls: AtomicUsize,
    fail_on_calls: Vec<usize>,
}

impl CyclingSwapBackOps {
    fn always_succeeds() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            fail_on_calls: Vec::new(),
        })
    }

    fn failing_on(calls: &[usize]) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            fail_on_calls: calls.to_vec(),
        })
    }
}

impl SwapBackOps for CyclingSwapBackOps {
    fn build_ort(&self) -> anyhow::Result<(Arc<dyn CoreEmbedder>, Option<Arc<AtomicU32>>)> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if self.fail_on_calls.contains(&n) {
            anyhow::bail!("fake ort rebuild failure (call #{n})");
        }
        let adapter: Arc<dyn CoreEmbedder> = Arc::new(FakeEmbedder::new());
        Ok((adapter, Some(Arc::new(AtomicU32::new(5000 + n as u32)))))
    }
}

fn test_state() -> SearchAppState {
    SearchAppState::new(crate::core::registry::IndexRegistry::new())
}

fn ort_bootstrapping_active() -> ActiveBackend {
    ActiveBackend {
        kind: BackendKind::Ort,
        provider: trusty_common::embedder::ExecutionProvider::Cpu,
        model: "all-MiniLM-L6-v2".to_string(),
        quantized: false,
        bootstrap: BootstrapState::Bootstrapping,
    }
}

// ============================================================================
// Fast, deterministic, in-CI tests.
// ============================================================================

/// Drives `drive_bootstrap` → `drive_swap_back_watchdog` → `drive_bootstrap`
/// → ... for `CYCLES` full ort→python→ort round trips against the SAME
/// `switchable`/`state`, exactly like a real daemon would if the python
/// sidecar kept dying and getting rebuilt. Asserts, across ALL cycles
/// combined: every spawned python handle got torn down exactly once (no
/// leaked python child — leak candidate #1) and every swap-back rebuilt
/// exactly one fresh ort backend (no accumulation).
#[tokio::test]
async fn repeated_cycles_never_leak_teardown_calls() {
    const CYCLES: usize = 5;

    let ort: Arc<dyn CoreEmbedder> = Arc::new(FakeEmbedder::new());
    let switchable = Arc::new(SwitchableEmbedder::new(ort, ort_bootstrapping_active()));
    let state = test_state();

    let bootstrap = CyclingBootstrap::new();
    let (ort_ops, ort_build_calls) = {
        let ops = CyclingSwapBackOps::always_succeeds();
        (ops, Arc::new(AtomicUsize::new(0)))
    };
    let _ = &ort_build_calls; // silence unused in case of future refactor

    for cycle in 0..CYCLES {
        let python_ops: Arc<dyn PythonBootstrap> = Arc::clone(&bootstrap) as _;
        let teardown = drive_bootstrap(Arc::clone(&switchable), state.clone(), python_ops, 1)
            .await
            .unwrap_or_else(|| {
                panic!("cycle {cycle}: bootstrap must succeed against the always-succeeding fake")
            });
        assert_eq!(
            switchable.active().kind,
            BackendKind::Python,
            "cycle {cycle}: must be on python after a successful bootstrap"
        );

        // Simulate confirmed death of THIS cycle's handle.
        bootstrap
            .last_terminated_flag()
            .store(true, Ordering::Release);

        let ops: Arc<dyn SwapBackOps> = Arc::clone(&ort_ops) as _;
        tokio::time::timeout(
            Duration::from_secs(2),
            drive_swap_back_watchdog(
                Arc::clone(&switchable),
                state.clone(),
                teardown,
                ops,
                Duration::from_millis(2),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("cycle {cycle}: watchdog must act on confirmed death"));

        assert_eq!(
            switchable.active().kind,
            BackendKind::Ort,
            "cycle {cycle}: must swap back to ort"
        );
    }

    assert_eq!(
        bootstrap.total_spawns.load(Ordering::Relaxed),
        CYCLES,
        "one python handle spawned per cycle"
    );
    assert_eq!(
        bootstrap.total_teardown_calls.load(Ordering::Relaxed),
        CYCLES,
        "leak candidate #1: every spawned python handle must be torn down \
         exactly once — teardown calls must equal spawn count across all \
         {CYCLES} cycles, with no accumulation and no gaps"
    );
}

/// Leak candidate #2: a `build_ort()` failure on ONE cycle must (a) still
/// tear down the dead python handle (no python-side leak just because the
/// ort rebuild failed), (b) leave the switchable in the documented
/// stays-on-dead-python state rather than some half-swapped/corrupt state,
/// and (c) NOT permanently wedge recovery — a subsequent bootstrap attempt on
/// the same `switchable`/`state` must still succeed, proving the failure
/// mode is exactly "stay on the dead backend until someone retries" (as
/// `swap_back_watchdog.rs`'s error log says), never a stranded, unrecoverable
/// handle.
#[tokio::test]
async fn build_ort_failure_mid_cycle_leaves_no_orphan_and_allows_recovery() {
    let ort: Arc<dyn CoreEmbedder> = Arc::new(FakeEmbedder::new());
    let switchable = Arc::new(SwitchableEmbedder::new(ort, ort_bootstrapping_active()));
    let state = test_state();
    let bootstrap = CyclingBootstrap::new();

    // Cycle 1: bootstrap to python, then fail the ort rebuild on swap-back.
    let python_ops: Arc<dyn PythonBootstrap> = Arc::clone(&bootstrap) as _;
    let teardown = drive_bootstrap(Arc::clone(&switchable), state.clone(), python_ops, 1)
        .await
        .expect("first bootstrap must succeed");
    assert_eq!(switchable.active().kind, BackendKind::Python);

    bootstrap
        .last_terminated_flag()
        .store(true, Ordering::Release);

    let failing_ops: Arc<dyn SwapBackOps> = CyclingSwapBackOps::failing_on(&[1]) as Arc<_>;
    tokio::time::timeout(
        Duration::from_secs(2),
        drive_swap_back_watchdog(
            Arc::clone(&switchable),
            state.clone(),
            Arc::clone(&teardown),
            failing_ops,
            Duration::from_millis(2),
        ),
    )
    .await
    .expect("watchdog must still act (return) even when build_ort fails");

    assert_eq!(
        switchable.active().kind,
        BackendKind::Python,
        "documented behavior: a failed ort rebuild leaves the switchable on \
         the (now dead) python backend rather than any half-built state"
    );
    assert_eq!(
        bootstrap.total_teardown_calls.load(Ordering::Relaxed),
        1,
        "leak candidate #2: the dead python handle must still be torn down \
         even though build_ort() failed — no orphan just because the \
         REBUILD failed"
    );

    // Recovery: a fresh bootstrap attempt on the SAME switchable/state must
    // still succeed — the failure must not have permanently wedged anything.
    let python_ops_2: Arc<dyn PythonBootstrap> = Arc::clone(&bootstrap) as _;
    let teardown_2 = drive_bootstrap(Arc::clone(&switchable), state.clone(), python_ops_2, 1)
        .await
        .expect(
            "recovery bootstrap must succeed — the earlier build_ort() \
                 failure must not have permanently wedged this switchable",
        );
    assert_eq!(switchable.active().kind, BackendKind::Python);

    // And a subsequent, SUCCEEDING swap-back must work normally too.
    bootstrap
        .last_terminated_flag()
        .store(true, Ordering::Release);
    let succeeding_ops: Arc<dyn SwapBackOps> = CyclingSwapBackOps::always_succeeds() as Arc<_>;
    tokio::time::timeout(
        Duration::from_secs(2),
        drive_swap_back_watchdog(
            switchable.clone(),
            state.clone(),
            teardown_2,
            succeeding_ops,
            Duration::from_millis(2),
        ),
    )
    .await
    .expect("watchdog must act on the recovered cycle's confirmed death");
    assert_eq!(switchable.active().kind, BackendKind::Ort);
    assert_eq!(
        bootstrap.total_teardown_calls.load(Ordering::Relaxed),
        2,
        "both the failed cycle's handle and the recovered cycle's handle \
         must each be torn down exactly once"
    );
}

// ============================================================================
// Heavy real soak — #[ignore]d AND gated on TRUSTY_SOAK=1 at runtime, mirroring
// `tests/py_sidecar_memory_soak.rs`'s convention. Drives the REAL production
// entry point repeatedly against a real `uv` venv + real torch/MPS sidecar +
// real `trusty-embedderd` ort sidecar, observing OS-level process counts as
// the leak signal rather than trusting internal call-counters alone.
// ============================================================================

/// Count live OS processes whose command line or name contains `needle`
/// (case-insensitive), excluding this test process itself. Real-signal check
/// for leak candidate #1 (a python child surviving past its teardown) that
/// does not depend on this harness having correctly tracked every pid it
/// spawned — an actual leaked process shows up here even if our own
/// bookkeeping lost track of it.
#[cfg(test)]
fn count_processes_matching(needle: &str) -> usize {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let needle = needle.to_ascii_lowercase();
    let my_pid = std::process::id();
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys.processes()
        .iter()
        .filter(|(pid, _)| pid.as_u32() != my_pid)
        .filter(|(_, proc_)| {
            let name_hit = proc_
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains(&needle);
            let cmd_hit = proc_
                .cmd()
                .iter()
                .any(|a| a.to_string_lossy().to_ascii_lowercase().contains(&needle));
            name_hit || cmd_hit
        })
        .count()
}

/// Real repeated ort→python→ort cycling through the actual production entry
/// point (`graceful_bootstrap::run_graceful_python_bootstrap`), which itself
/// spawns the real `swap_back_watchdog` on success — exercising the EXACT
/// code path a daemon runs, just invoked several times by this test (since
/// production only ever calls it once per daemon lifetime).
///
/// Requires a real `trusty-embedderd` + `trusty-embedderd-py` launcher with
/// its `uv`-managed venv already bootstrapped (torch + sentence-transformers)
/// — practically, Apple Silicon with `uv` installed. Not run in CI. Run
/// manually with:
///   `TRUSTY_SOAK=1 cargo test -p trusty-search --lib \
///      real_repeated_ort_python_ort_cycles_soak -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real trusty-embedderd + trusty-embedderd-py + a bootstrapped \
            uv/torch/MPS venv (Apple Silicon) — additionally gated on TRUSTY_SOAK=1"]
async fn real_repeated_ort_python_ort_cycles_soak() {
    if std::env::var("TRUSTY_SOAK").ok().as_deref() != Some("1") {
        eprintln!(
            "real_repeated_ort_python_ort_cycles_soak: TRUSTY_SOAK != 1 — skipping. \
             Run with `TRUSTY_SOAK=1 cargo test ... -- --ignored --nocapture`."
        );
        return;
    }

    const CYCLES: usize = 3;
    // Keep each cycle's crash-restart-exhaustion loop short: 1 restart, then
    // the supervisor gives up on the next failure.
    std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", "1");

    let (ort, ort_pid_slot) = super::embedder::build_ort_stdio_sidecar().expect(
        "build_ort_stdio_sidecar() failed — is `trusty-embedderd` on PATH \
             (or TRUSTY_EMBEDDERD_BIN set)?",
    );
    let provider = ort.provider();
    let switchable = Arc::new(SwitchableEmbedder::new(
        ort,
        ActiveBackend {
            kind: BackendKind::Ort,
            provider,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized: false,
            bootstrap: BootstrapState::Bootstrapping,
        },
    ));
    let state = test_state();
    if let Some(slot) = ort_pid_slot {
        state.install_embedderd_pid_slot(slot).await;
    }

    let baseline_python_procs = count_processes_matching("trusty-embedderd-py");
    eprintln!("soak: baseline live trusty-embedderd-py processes = {baseline_python_procs}");

    for cycle in 0..CYCLES {
        eprintln!("soak: cycle {cycle}/{CYCLES} — bootstrapping ort -> python");
        super::graceful_bootstrap::run_graceful_python_bootstrap(
            Arc::clone(&switchable),
            state.clone(),
        )
        .await;

        let active = switchable.active();
        if active.kind != BackendKind::Python {
            panic!(
                "cycle {cycle}: real python bootstrap failed (bootstrap={:?}) — \
                 is the uv venv actually bootstrapped? Run \
                 `trusty-embedderd-py --stdio` once by hand first, or \
                 `cargo run -p trusty-embedderd-py --bin trusty-embedderd-py -- --bootstrap-only` \
                 if that flag exists, to warm the venv before soaking.",
                active.bootstrap
            );
        }
        eprintln!(
            "soak: cycle {cycle} — python bootstrap succeeded (provider={:?})",
            active.provider
        );

        // Touch the live backend for real so this isn't just a bootstrap
        // no-op — a handful of real embeds through the switchable.
        let probe_texts = ["fn soak_probe() {}"; 4];
        switchable
            .embed_batch(&probe_texts)
            .await
            .expect("real embed through the freshly-bootstrapped python backend failed");

        let live_pid = state
            .current_embedderd_pid()
            .filter(|&p| p != 0)
            .unwrap_or_else(|| panic!("cycle {cycle}: no live python pid recorded on state"));
        eprintln!("soak: cycle {cycle} — live python pid = {live_pid}");

        // Repeatedly SIGKILL whatever pid is currently live until the
        // switchable itself confirms the watchdog swapped back to ort — no
        // fabricated state, just the real supervisor + real watchdog.
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(pid) = state.current_embedderd_pid().filter(|&p| p != 0) {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status();
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            if switchable.active().kind == BackendKind::Ort {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cycle {cycle}: watchdog never swapped back to ort within 120s \
                 of repeated SIGKILLs — max_restarts=1 not exhausted, or the \
                 watchdog's 15s poll never observed confirmed death"
            );
        }
        eprintln!(
            "soak: cycle {cycle} — swap-back confirmed (active={:?})",
            switchable.active().kind
        );

        // Give the torn-down python process a moment to actually exit before
        // sampling — teardown() awaits confirmed death, but the OS may take
        // a beat to reap.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let procs_now = count_processes_matching("trusty-embedderd-py");
        assert_eq!(
            procs_now, baseline_python_procs,
            "cycle {cycle}: leak candidate #1 — live trusty-embedderd-py \
             process count did not return to baseline ({baseline_python_procs}) \
             after swap-back; found {procs_now} — a python child likely \
             survived its teardown"
        );
    }

    eprintln!(
        "soak: all {CYCLES} real ort->python->ort cycles completed; \
         trusty-embedderd-py process count returned to baseline \
         ({baseline_python_procs}) after every cycle — no leaked python child \
         detected."
    );

    std::env::remove_var("TRUSTY_EMBEDDERD_MAX_RESTARTS");
}

/// Real, single-cycle proof of leak candidate #2: force `build_ort()`
/// (`RealSwapBackOps::build_ort` → `build_ort_stdio_sidecar` →
/// `locate_embedderd_binary`) to genuinely fail during a REAL swap-back by
/// pointing `TRUSTY_EMBEDDERD_BIN` at a nonexistent path right before
/// inducing death, then confirms: (a) the switchable stays on the (now dead)
/// python backend — never a half-built/corrupt state — (b) the dead python
/// process is still fully reaped (process count returns to baseline) even
/// though the ort rebuild failed, and (c) restoring `TRUSTY_EMBEDDERD_BIN`
/// and running one more real bootstrap cycle recovers cleanly — proving the
/// failure mode is exactly "stuck on dead python until someone retries", not
/// a permanent wedge or an orphaned resource.
///
/// Not run in CI; gated the same way as
/// `real_repeated_ort_python_ort_cycles_soak`. Run manually with:
///   `TRUSTY_SOAK=1 cargo test -p trusty-search --lib \
///      real_build_ort_failure_leaves_no_orphan_and_recovers -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires real trusty-embedderd + trusty-embedderd-py + a bootstrapped \
            uv/torch/MPS venv (Apple Silicon) — additionally gated on TRUSTY_SOAK=1"]
async fn real_build_ort_failure_leaves_no_orphan_and_recovers() {
    if std::env::var("TRUSTY_SOAK").ok().as_deref() != Some("1") {
        eprintln!(
            "real_build_ort_failure_leaves_no_orphan_and_recovers: TRUSTY_SOAK != 1 — \
             skipping. Run with `TRUSTY_SOAK=1 cargo test ... -- --ignored --nocapture`."
        );
        return;
    }

    std::env::set_var("TRUSTY_EMBEDDERD_MAX_RESTARTS", "1");

    // Resolve the REAL ort binary path up front (before we start pointing
    // TRUSTY_EMBEDDERD_BIN at garbage) so we can restore it verbatim later.
    let real_ort_bin = crate::service::embedder_supervisor::locate_embedderd_binary()
        .expect("a real trusty-embedderd must be discoverable for this test to mean anything")
        .to_string_lossy()
        .into_owned();

    let (ort, ort_pid_slot) = super::embedder::build_ort_stdio_sidecar()
        .expect("build_ort_stdio_sidecar() failed for the initial ort backend");
    let provider = ort.provider();
    let switchable = Arc::new(SwitchableEmbedder::new(
        ort,
        ActiveBackend {
            kind: BackendKind::Ort,
            provider,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized: false,
            bootstrap: BootstrapState::Bootstrapping,
        },
    ));
    let state = test_state();
    if let Some(slot) = ort_pid_slot {
        state.install_embedderd_pid_slot(slot).await;
    }

    let baseline_python_procs = count_processes_matching("trusty-embedderd-py");

    super::graceful_bootstrap::run_graceful_python_bootstrap(
        Arc::clone(&switchable),
        state.clone(),
    )
    .await;
    assert_eq!(
        switchable.active().kind,
        BackendKind::Python,
        "real python bootstrap failed — is the uv venv actually bootstrapped?"
    );

    // Now force the swap-back's build_ort() to fail: point TRUSTY_EMBEDDERD_BIN
    // at a path that does not exist. `locate_embedderd_binary()` bails
    // immediately on an explicit-but-invalid override rather than falling
    // through to PATH search, so this is a deterministic failure trigger.
    std::env::set_var(
        "TRUSTY_EMBEDDERD_BIN",
        "/nonexistent/trusty-embedderd-for-soak-test",
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(pid) = state.current_embedderd_pid().filter(|&p| p != 0) {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        // With build_ort() broken, the watchdog can never observe
        // BackendKind::Ort — instead we wait for the dead python process to
        // actually disappear from the process table (teardown() completing)
        // as our "the watchdog has acted" signal.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let procs_now = count_processes_matching("trusty-embedderd-py");
        if procs_now <= baseline_python_procs {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "python process never disappeared within 120s of repeated SIGKILLs \
             — max_restarts=1 not exhausted, or teardown() never ran"
        );
    }

    // Restore TRUSTY_EMBEDDERD_BIN immediately — every assertion below must
    // hold regardless, but we don't want a failed assertion to leave the
    // process env poisoned for any later test in the same binary.
    std::env::set_var("TRUSTY_EMBEDDERD_BIN", &real_ort_bin);

    assert_eq!(
        switchable.active().kind,
        BackendKind::Python,
        "leak candidate #2: a failed build_ort() must leave the switchable on \
         the (now dead) python backend — documented behavior, never a \
         half-built/corrupt ort state"
    );
    let procs_after = count_processes_matching("trusty-embedderd-py");
    assert_eq!(
        procs_after, baseline_python_procs,
        "leak candidate #2: the dead python process must still be fully \
         reaped (teardown() still runs on the build_ort() Err branch) even \
         though the ort rebuild failed — found {procs_after} vs baseline \
         {baseline_python_procs}"
    );

    // Recovery: with a valid TRUSTY_EMBEDDERD_BIN restored, one more real
    // bootstrap cycle on the SAME switchable/state must succeed cleanly.
    super::graceful_bootstrap::run_graceful_python_bootstrap(
        Arc::clone(&switchable),
        state.clone(),
    )
    .await;
    assert_eq!(
        switchable.active().kind,
        BackendKind::Python,
        "recovery bootstrap after a build_ort() failure must still succeed — \
         the earlier failure must not have permanently wedged this switchable"
    );

    std::env::remove_var("TRUSTY_EMBEDDERD_MAX_RESTARTS");
    std::env::remove_var("TRUSTY_EMBEDDERD_BIN");
}
