//! Every test that MUTATES `TRUSTY_MAX_INDEX_FILES`, isolated into its own test
//! binary (issue #4356, following the #3769 precedent).
//!
//! Why: `TRUSTY_MAX_INDEX_FILES` is process-global, and
//! `service::reindex::runner` reads it through
//! [`trusty_search::service::index_budget::IndexBudget::from_env`] on EVERY
//! reindex. These two tests hold it at `"1"` for the length of a whole reindex.
//! Run from inside the `trusty-search` LIB test binary, that window overlapped
//! roughly nineteen non-serial tests in `service::reindex::tests` that each
//! drive a reindex of their own — every one of them would have had its walk
//! refused as over-budget and failed for a reason that had nothing to do with
//! what it was testing. `#[serial]` does not close that: it orders these two
//! against each other and against other `#[serial]` tests, not against the
//! non-serial majority. The second, subtler exposure is the one #3769 names —
//! `setenv` reallocates the C `environ` array, so a concurrent `getenv`
//! anywhere in the process can tear regardless of any test-level ordering.
//!
//! What: a test binary is its own PROCESS, so the lib binary now has no writer
//! of this variable at all. Inside here the two tests are `#[serial]` against
//! each other and are the only tests present. Both keep their original
//! assertions — no coverage dropped, no `#[ignore]`, no widened tolerance. They
//! drive the reindex through the public `spawn_reindex` + status-poll seam
//! (the same one `tests/reindex_stage_profile.rs` uses) rather than the
//! `pub(crate)` awaitable variant the in-crate versions called.
//!
//! The third env-touching test, `index_budget::tests::
//! malformed_value_falls_back_to_default`, stayed in the lib binary because it
//! no longer writes env at all — it calls the pure `parse_limit` directly.
//!
//! Test: `reindex_refuses_walk_over_file_budget`,
//! `reindex_budget_is_inclusive_at_the_boundary` below.

use std::sync::Arc;
use std::time::Duration;

use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, StageStatus};
use trusty_search::service::index_budget::ENV_MAX_INDEX_FILES;
use trusty_search::service::reindex::{spawn_reindex, ReindexProgress, ReindexStatus};

/// RAII override of `TRUSTY_MAX_INDEX_FILES` (#4356).
///
/// Env is process-global. Every test that uses this must be
/// `#[serial_test::serial]`, and this binary must stay the only place in the
/// crate that writes the variable — see the module docs.
struct MaxIndexFilesEnvGuard(Option<String>);

impl MaxIndexFilesEnvGuard {
    fn set(v: &str) -> Self {
        let prior = std::env::var(ENV_MAX_INDEX_FILES).ok();
        std::env::set_var(ENV_MAX_INDEX_FILES, v);
        Self(prior)
    }
}

impl Drop for MaxIndexFilesEnvGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(v) => std::env::set_var(ENV_MAX_INDEX_FILES, v),
            None => std::env::remove_var(ENV_MAX_INDEX_FILES),
        }
    }
}

/// Stage `count` walkable `.rs` files under a fresh tempdir and return both.
fn budget_fixture(count: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    for i in 0..count {
        std::fs::write(root.join(format!("f{i}.rs")), format!("fn f{i}() {{}}\n")).unwrap();
    }
    (tmp, root)
}

/// One reindex run: the progress handle, the index handle, and every frame a
/// LIVE SSE subscriber received.
struct BudgetRun {
    progress: Arc<ReindexProgress>,
    handle: Arc<IndexHandle>,
    /// Frames drained from `progress.sender`, subscribed BEFORE the run started.
    ///
    /// Why this and not `progress.events`: the replay buffer only holds what
    /// `ReindexProgress::push` wrote. `ReindexTerminationGuard::drop` cannot
    /// `.await`, so it broadcasts WITHOUT touching the buffer — a guard frame is
    /// invisible to a reader of `events` and visible only here. That asymmetry
    /// is exactly what let a double-emit go unnoticed.
    live: Vec<String>,
}

/// Run one reindex against `root` under index id `id`.
///
/// `spawn_reindex` is fire-and-forget (the `pub(crate)` awaitable variant is
/// not reachable from an integration binary), so poll for a terminal status.
/// The fixtures here are two or three tiny files, so the ceiling is generous
/// relative to the real work and a breach means a genuine hang, not slowness.
async fn run_budget_reindex_with_handle(id: &str, root: &std::path::Path) -> BudgetRun {
    let indexer = CodeIndexer::new(id.to_string(), root.to_path_buf());
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    let progress = Arc::new(ReindexProgress::new());
    let mut rx = progress.sender.subscribe();
    spawn_reindex(Arc::clone(&handle), Arc::clone(&progress), false);

    for _ in 0..600 {
        if progress.status.load() != ReindexStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_ne!(
        progress.status.load(),
        ReindexStatus::Running,
        "reindex never reached a terminal status"
    );
    // The guard's frame is broadcast from `Drop`, which runs as the task
    // unwinds — after the status store this loop watched for. Give the task a
    // moment to finish returning before draining, or the very frame this run
    // exists to count could still be in flight.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut live = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        live.push(frame);
    }

    let diag_error = handle.walk_diagnostics.read().await.last_walk_error.clone();
    if progress.status.load() == ReindexStatus::Failed {
        let reason = diag_error.expect("a refused walk must record last_walk_error");
        assert!(
            reason.contains("TRUSTY_MAX_INDEX_FILES"),
            "the refusal must name the ceiling that raises it: {reason}"
        );
    }
    BudgetRun {
        progress,
        handle,
        live,
    }
}

/// `run_budget_reindex_with_handle` for callers that only need the progress.
async fn run_budget_reindex(id: &str, root: &std::path::Path) -> Arc<ReindexProgress> {
    run_budget_reindex_with_handle(id, root).await.progress
}

/// #4356: a walk over the file budget refuses the whole reindex instead of
/// indexing a truncated prefix.
///
/// Why: before this, the only ceiling was `TRUSTY_MAX_CHUNKS`, which DROPS
/// chunks past the cap and still reports `complete`. An index truncated that
/// way returns empty results that a caller cannot tell from a legitimate miss.
/// What: three walkable files against a one-file budget. Asserts the terminal
/// status is `Failed`, nothing was indexed, no lane is stranded mid-walk, and
/// exactly one fatal SSE frame is emitted, naming both remedies.
/// Test: this test. It fails against the pre-fix commit, where the same
/// fixture reaches `ReindexStatus::Complete` with `indexed == 3`.
#[tokio::test]
#[serial_test::serial]
async fn reindex_refuses_walk_over_file_budget() {
    let _guard = MaxIndexFilesEnvGuard::set("1");
    let (_tmp, root) = budget_fixture(3);

    let run = run_budget_reindex_with_handle("budget-over", &root).await;
    let (progress, handle) = (&run.progress, &run.handle);

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "an over-budget walk must fail, never complete truncated"
    );
    assert_eq!(
        progress.indexed_count(),
        0,
        "the refusal happens before any file is committed"
    );

    // #4356: nothing was walked, so no lane may be left mid-flight. A stranded
    // `lexical: InProgress` reads as an index stuck mid-walk (#5575) and would
    // force `/health` to `degraded` for a refusal that worked as designed.
    let stages = handle.stages.read().await.clone();
    assert_ne!(
        stages.lexical.status,
        StageStatus::InProgress,
        "the lexical lane must not be stranded mid-walk by a refusal"
    );
    assert_eq!(stages.lexical.status, StageStatus::Failed);
    assert_eq!(stages.lifecycle_status(), "failed");

    // Read the LIVE stream, not the replay buffer: `ReindexTerminationGuard::
    // drop` broadcasts without writing to `events`, so a second frame is
    // invisible there. See `BudgetRun::live`.
    let fatal: Vec<&String> = run
        .live
        .iter()
        .filter(|e| e.contains("\"event\":\"error\"") && e.contains("\"fatal\":true"))
        .collect();
    assert_eq!(
        fatal.len(),
        1,
        "a refusal is ONE failure and owes an SSE subscriber exactly one \
         terminal frame. A second one is the still-armed termination guard \
         firing on drop with its generic \"exited unexpectedly (panic or \
         cancellation)\" text, which the CLI prints verbatim beneath the real \
         message — sending an operator who narrowed their index correctly to \
         hunt a panic backtrace that does not exist. Frames: {fatal:?}"
    );
    assert!(
        fatal[0].contains("TRUSTY_MAX_INDEX_FILES") && fatal[0].contains("exclude_globs"),
        "the frame must name both remedies: {}",
        fatal[0]
    );
    // The refusal must still reach a LATE subscriber, which reads the replay
    // buffer instead. Disarming the guard must not have cost that.
    let replayed = progress.events.lock().await;
    assert!(
        replayed
            .iter()
            .any(|e| e.contains("\"fatal\":true") && e.contains("TRUSTY_MAX_INDEX_FILES")),
        "the refusal must also be replayable to a late subscriber: {replayed:?}"
    );
}

/// #4356: the budget is inclusive — exactly at the ceiling still indexes.
///
/// Why: an off-by-one that made the guard fire AT the limit would refuse every
/// index sized exactly to its budget, and the enforcement test above cannot
/// tell that apart from correct behaviour.
/// What: two walkable files against a two-file budget, then the same fixture
/// against a one-file budget.
/// Test: this test. The `Complete` half also proves the guard does not break
/// the ordinary under-budget path.
#[tokio::test]
#[serial_test::serial]
async fn reindex_budget_is_inclusive_at_the_boundary() {
    let (_tmp, root) = budget_fixture(2);

    {
        let _guard = MaxIndexFilesEnvGuard::set("2");
        let progress = run_budget_reindex("budget-exact", &root).await;
        assert_eq!(
            progress.status.load(),
            ReindexStatus::Complete,
            "exactly at the ceiling must still index"
        );
        assert_eq!(progress.indexed_count(), 2);
    }

    let _guard = MaxIndexFilesEnvGuard::set("1");
    let progress = run_budget_reindex("budget-one-over", &root).await;
    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "one file over the ceiling must refuse"
    );
}
