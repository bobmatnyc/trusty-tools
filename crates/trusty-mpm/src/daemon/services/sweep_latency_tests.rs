//! 🔴 #7965 REGRESSION: a running sweep never delays the request path.
//!
//! Why these two and not more: the reported failure had two independent sources —
//! the merged-PR reclaim's liveness probe and the in-project hygiene pass — and
//! the operator proved they were independent by switching them off one at a time
//! (`TRUSTY_MPM_WORKTREE_RECLAIM=off` alone still left ~5 s per request nine
//! minutes after boot; adding `TRUSTY_MPM_INPROJECT_HYGIENE=0` brought `/health`
//! to 0.23 s). Each test therefore drives ONE sweep with a subprocess that never
//! answers, and asserts two things: the sweep itself finishes, and `/health`
//! answers inside the budget the CLIENT actually allows.
//!
//! # Why 250 ms and not a round second
//!
//! `crate::core::discovery`'s daemon probe times out at 500 ms, and with
//! `TRUSTY_MPM_URL` set the CLI refuses the discovery fallback — so any
//! request-path latency past 500 ms makes every `tm` command report the daemon
//! unreachable. 250 ms is half of that, which leaves the assertion meaningful on a
//! loaded CI box without letting through a latency the client would reject.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::daemon::rpc::core_ops;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedError, ManagedTmuxDriver};

/// The ceiling `/health` must stay under while a sweep runs. See the module doc.
const HEALTH_BUDGET: Duration = Duration::from_millis(250);

/// A tmux driver whose liveness probe never answers in time.
///
/// Why a driver rather than a sleeping `FreshProbes` closure: the probe seam the
/// sweep actually uses reaches tmux through this trait, and a closure would prove
/// only that a closure can sleep.
struct WedgedTmux;

impl ManagedTmuxDriver for WedgedTmux {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    /// The whole point of this double: tmux that does not answer.
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        // Long enough to prove the 3 s ceiling fires; short enough that the
        // runtime's shutdown — which waits for uncancellable blocking tasks —
        // does not dominate the suite.
        std::thread::sleep(Duration::from_secs(10));
        Ok(Vec::new())
    }
}

/// Measure the worst `/health` latency observed while `work` runs.
///
/// What: polls the real `mpm.health` body — the same one `GET /health` returns —
/// every 20 ms until `work` finishes, and reports the slowest answer.
async fn worst_health_latency_during<F>(state: &Arc<DaemonState>, work: F) -> Duration
where
    F: std::future::Future<Output = ()>,
{
    let probe_state = Arc::clone(state);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe_done = Arc::clone(&done);
    let prober = tokio::spawn(async move {
        let mut worst = Duration::ZERO;
        while !probe_done.load(std::sync::atomic::Ordering::Relaxed) {
            let t0 = Instant::now();
            let _ = core_ops::health(&probe_state).await;
            worst = worst.max(t0.elapsed());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        worst
    });
    work.await;
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    prober.await.expect("the health prober must not panic")
}

/// 🔴 #7965: a liveness probe that never answers bounds itself, and `/health`
/// stays inside the client's budget throughout.
///
/// Pre-fix, `workspace_claims` ran `ensure_server_up` + `list_sessions` inline on
/// the calling thread with no ceiling, so this call took the full 30 seconds and
/// the assertion below failed on elapsed time. Post-fix the probe is bounded by
/// `TMUX_PROBE_TIMEOUT` and every claim stays LIVE, which REFUSES every delete —
/// the #2919/#5661/#7232 fail-closed contract, unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wedged_liveness_probe_neither_hangs_the_sweep_nor_delays_health() {
    let dir = tempfile::tempdir().expect("scratch framework root");
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            dir.path().to_path_buf(),
            Arc::new(WedgedTmux),
        )
        .await,
    );
    let mgr = state.session_manager().await;

    let started = Instant::now();
    // The fail-closed half — every claim stays LIVE when the probe cannot answer —
    // is asserted against seeded records by
    // `a_slow_tmux_probe_is_bounded_and_leaves_every_claim_live`. This test owns
    // the timing half.
    let worst = worst_health_latency_during(&state, async {
        let _ = mgr.workspace_claims(None).await;
    })
    .await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(6),
        "the liveness probe must bound itself; the pass took {elapsed:?} (#7965)"
    );
    assert!(
        worst < HEALTH_BUDGET,
        "/health took {worst:?} while the sweep probed tmux; budget is {HEALTH_BUDGET:?} (#7965)"
    );
}

/// 🔴 #7965: a hygiene base whose `git fetch` blocks is abandoned, and `/health`
/// stays inside the client's budget throughout.
///
/// Pre-fix every `git` in the sweep was `Command::output()` with no ceiling, so a
/// `fetch` blocked on a credential prompt — the shape `sample` caught — pinned a
/// blocking-pool thread for the life of the daemon and the pass below never
/// returned.
///
/// The wedge is a remote on a non-routable RFC-1918 address, so the fetch blocks
/// in `connect(2)` until the OS gives up (~75 s on macOS) — not a `git` shim on
/// `PATH`, which is process-global and would hang every other test in this binary
/// that shells out to git. Only THIS repository's fetch blocks; every other git
/// command in the pass runs normally, which is also closer to the reported
/// failure. Measured against the pre-fix code, the pass below took 75.4 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wedged_hygiene_fetch_neither_hangs_the_sweep_nor_delays_health() {
    use crate::daemon::managed_routes::inproject_hygiene;

    let dir = tempfile::tempdir().expect("scratch root");
    let repos_root = dir.path().join("repos");
    let base = repos_root.join("owner").join("repo");
    std::fs::create_dir_all(&base).expect("base clone");
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git://10.255.255.1:9418/x.git"],
    ] {
        let out = trusty_common::git::command_in(&base)
            .args(&args)
            .output()
            .expect("git must be available to set the fixture up");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    assert!(
        !base.join(".git").join("FETCH_HEAD").exists(),
        "the fixture must be due a fetch"
    );

    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    let started = Instant::now();
    let worst = worst_health_latency_during(&state, async {
        let root = repos_root.clone();
        tokio::task::spawn_blocking(move || {
            // A 2 s per-command ceiling rather than the production 60 s: what is
            // under test is that a ceiling FIRES, not how long it is.
            inproject_hygiene::run_hygiene_for_all_bases_within(
                &root,
                Duration::from_secs(60),
                Duration::from_secs(2),
            );
        })
        .await
        .expect("the hygiene pass must not panic");
    })
    .await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(30),
        "a blocked fetch must be killed, not waited on; the pass took {elapsed:?} (#7965)"
    );
    assert!(
        worst < HEALTH_BUDGET,
        "/health took {worst:?} while hygiene ran; budget is {HEALTH_BUDGET:?} (#7965)"
    );
}

/// The worst gap between two `/health` answers while `work` runs, plus `work`'s
/// output.
///
/// Why a gap and not a call latency: on a `current_thread` runtime a task that
/// blocks the thread stalls the prober BETWEEN calls, so timing each call alone
/// would report a fast `/health` throughout a multi-second stall.
/// What: polls `mpm.health` every 20 ms and reports the longest time from one
/// answer to the next, minus that 20 ms pause. The clock runs from answer to
/// answer, so a stall that lands during the pause is counted too (#7965).
async fn worst_health_gap_during<F, T>(state: &Arc<DaemonState>, work: F) -> (Duration, T)
where
    F: std::future::Future<Output = T>,
{
    const PAUSE: Duration = Duration::from_millis(20);
    let probe_state = Arc::clone(state);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe_done = Arc::clone(&done);
    let prober = tokio::spawn(async move {
        let mut worst = Duration::ZERO;
        let mut last_answer = Instant::now();
        while !probe_done.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = core_ops::health(&probe_state).await;
            let answered = Instant::now();
            worst = worst.max((answered - last_answer).saturating_sub(PAUSE));
            last_answer = answered;
            tokio::time::sleep(PAUSE).await;
        }
        worst
    });
    let out = work.await;
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    (prober.await.expect("the health prober must not panic"), out)
}

/// 🔴 #7965: an orphan-GC worktree pass stuck on a `git status` that never
/// answers keeps `/health` inside the client's budget, returns once the git
/// ceiling fires, and keeps the worktree it could not inspect.
///
/// Why `current_thread`: it is the sharpest form of "no git subprocess runs on a
/// runtime worker". With one runtime thread, a git call made inline on the async
/// task stalls every other task — the `/health` prober included — for as long as
/// git does. Pre-fix, Phase 1.5's dirty gate ran inline, so this pass held the
/// thread until the wedge released at 8 s, then found the worktree clean and
/// removed it.
///
/// The wedge is a FIFO in place of the checkout's `.git/info/exclude` (see
/// `GitWorktreeFixture::wedge_status`): only THIS repository's `git status`
/// blocks, and no PATH shim leaks into sibling tests.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn a_wedged_git_status_neither_hangs_the_orphan_sweep_nor_delays_health() {
    use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
    use crate::session_manager::{DirtyWorktreePolicy, SessionManager};

    let dir = tempfile::tempdir().expect("scratch framework root");
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    let store_dir = tempfile::tempdir().expect("scratch session store");
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("wedged");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    let _wedge = fx.wedge_status(Duration::from_secs(8));

    let started = Instant::now();
    let (worst, outcome) = worst_health_gap_during(
        &state,
        mgr.prune_orphaned_worktrees_within(
            &fx.repos_root,
            &[],
            false,
            DirtyWorktreePolicy::Skip,
            &[],
            Duration::from_secs(2),
        ),
    )
    .await;
    let elapsed = started.elapsed();
    let outcome = outcome.expect("the pass must not error");

    assert!(
        worst < HEALTH_BUDGET,
        "/health went {worst:?} unanswered while the orphan sweep ran; budget is \
         {HEALTH_BUDGET:?} (#7965)"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "the git ceiling must end the pass, not the wedge's 8 s release; took {elapsed:?} (#7965)"
    );
    assert!(
        wt.exists() && outcome.removed.is_empty(),
        "a worktree whose dirty check timed out must be kept; removed {:?} (#7965)",
        outcome.removed
    );
    assert!(
        outcome.skipped_dirty.iter().any(|d| d.path == wt),
        "the timed-out candidate must be reported as skipped, not silently dropped; got {:?}",
        outcome.skipped_dirty
    );
}
