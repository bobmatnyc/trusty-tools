//! The graceful python bootstrap's retry detail and its stop handle (#9949).
//!
//! Why: the bootstrap now keeps retrying in the background after its startup
//! attempts fail, instead of pinning the daemon to ort for its whole uptime.
//! `/health` therefore needs more than a state word: it must say why the last
//! attempt failed and when the next one runs. Daemon shutdown also needs a way
//! to stop that long-lived retry task and wait until an attempt in flight has
//! torn its python sidecar down.
//! What: [`BootstrapControl`] lives inside `SwitchableEmbedder`, beside the
//! `BootstrapState` it elaborates. It holds a wait-free [`BootstrapReport`]
//! snapshot, the cancellation token the retry loop selects on, and the retry
//! task's join handle so [`BootstrapControl::shutdown`] can wait for it.
//! Test: `shutdown_waits_for_the_attached_task`,
//! `shutdown_aborts_a_task_that_outlives_the_bound`,
//! `health_view_reports_the_next_retry_time` below; the retry loop's own tests
//! are in `commands/start/tests_9949.rs`.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use arc_swap::ArcSwap;
use serde::Serialize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long daemon shutdown waits for the bootstrap task to finish its
/// teardown before aborting it. A torn-down attempt only kills one child, so
/// this is short; it is charged to the SIGTERM window the index flush also uses.
pub const BOOTSTRAP_STOP_BUDGET: Duration = Duration::from_secs(5);

/// What the python bootstrap last did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootstrapReport {
    /// Attempts started so far, startup and background together.
    pub attempts: u32,
    /// Why the most recent attempt failed. `None` before the first failure
    /// and again after a success.
    pub last_error: Option<String>,
    /// When the next background attempt starts. `None` while an attempt runs,
    /// after a success, and after a terminal failure.
    pub next_retry_at: Option<SystemTime>,
}

/// The `/health` view of the python bootstrap (#9949), serialized as the
/// `python_bootstrap` object.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PythonBootstrapHealth {
    /// Same string as the top-level `embedder_bootstrap` field.
    pub state: &'static str,
    /// Attempts started so far.
    pub attempts: u32,
    /// Why the most recent attempt failed, if it did.
    pub last_error: Option<String>,
    /// RFC 3339 UTC time of the next background attempt, when one is scheduled.
    pub next_retry_at: Option<String>,
}

impl PythonBootstrapHealth {
    /// Combine the state string `/health` already computed with `report`.
    pub fn new(state: &'static str, report: &BootstrapReport) -> Self {
        Self {
            state,
            attempts: report.attempts,
            last_error: report.last_error.clone(),
            next_retry_at: report
                .next_retry_at
                .map(|at| chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339()),
        }
    }
}

/// Retry detail plus the stop handle for the graceful python bootstrap.
///
/// Why: see the module docs.
/// What: `report` is an `ArcSwap` so `/health` reads it without a lock;
/// `cancel` is cancelled once, by [`Self::shutdown`]; `task` holds the
/// detached bootstrap task's handle once the daemon attaches it.
/// Test: see the module-level `Test:` pointer.
pub struct BootstrapControl {
    report: ArcSwap<BootstrapReport>,
    cancel: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Default for BootstrapControl {
    fn default() -> Self {
        Self {
            report: ArcSwap::from_pointee(BootstrapReport::default()),
            cancel: CancellationToken::new(),
            task: Mutex::new(None),
        }
    }
}

impl BootstrapControl {
    /// Snapshot the current report. Wait-free.
    pub fn report(&self) -> Arc<BootstrapReport> {
        self.report.load_full()
    }

    /// Replace the report with `update(current)`, retried against the latest
    /// value if another writer lands first.
    pub fn update_report(&self, update: impl Fn(&BootstrapReport) -> BootstrapReport) {
        self.report.rcu(|current| Arc::new(update(current)));
    }

    /// The token the retry loop selects on; cancelled by [`Self::shutdown`].
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Record the detached bootstrap task so [`Self::shutdown`] can wait for it.
    pub fn attach_task(&self, task: JoinHandle<()>) {
        let previous = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(task);
        if let Some(previous) = previous {
            // One bootstrap task per daemon; a second attach replaces a stale one.
            previous.abort();
        }
    }

    /// Stop the retry loop and wait up to `bound` for the task to finish.
    ///
    /// Why: an attempt in flight owns a just-spawned python child. Cancelling
    /// lets the loop tear that child down cooperatively before the daemon
    /// exits through `process::exit`, which runs no `Drop`.
    /// What: cancels the token, then awaits the attached task for at most
    /// `bound`. A task still running at the bound is aborted and then awaited
    /// until the abort completes, so the in-flight spawn (whose child is
    /// `kill_on_drop`) has been dropped when this returns. Returns `true` when
    /// the task finished inside the bound or none was attached.
    /// Test: `shutdown_waits_for_the_attached_task`,
    /// `shutdown_aborts_a_task_that_outlives_the_bound`.
    pub async fn shutdown(&self, bound: Duration) -> bool {
        self.cancel.cancel();
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(mut task) = task else {
            return true;
        };
        if tokio::time::timeout(bound, &mut task).await.is_ok() {
            return true;
        }
        tracing::warn!(
            "graceful python bootstrap did not stop within {}s of shutdown — aborting it",
            bound.as_secs()
        );
        task.abort();
        // #9949: an abort only schedules the drop; wait for it so the child is
        // killed before the caller reaches `process::exit`. The cancelled
        // `JoinError` is the expected outcome.
        let _ = task.await;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_waits_for_the_attached_task() {
        let control = BootstrapControl::default();
        let token = control.cancel_token();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        control.attach_task(tokio::spawn(async move {
            token.cancelled().await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));

        assert!(control.shutdown(Duration::from_secs(5)).await);
        assert!(finished.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            control.shutdown(Duration::from_secs(5)).await,
            "a second shutdown has no task left to wait on"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_aborts_a_task_that_outlives_the_bound() {
        let control = BootstrapControl::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        control.attach_task(tokio::spawn(async move {
            // Ignores cancellation; only an abort drops `tx`.
            let _tx = tx;
            std::future::pending::<()>().await;
        }));

        assert!(!control.shutdown(Duration::from_secs(5)).await);
        assert!(
            rx.await.is_err(),
            "the aborted task must have dropped its state"
        );
    }

    #[test]
    fn health_view_reports_the_next_retry_time() {
        let report = BootstrapReport {
            attempts: 3,
            last_error: Some("probe timed out".to_string()),
            next_retry_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(120)),
        };
        let view = PythonBootstrapHealth::new("retrying", &report);
        assert_eq!(view.state, "retrying");
        assert_eq!(view.attempts, 3);
        assert_eq!(view.last_error.as_deref(), Some("probe timed out"));
        assert_eq!(
            view.next_retry_at.as_deref(),
            Some("1970-01-01T00:02:00+00:00")
        );
    }
}
