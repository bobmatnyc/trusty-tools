//! Background work and database reads for `tga tui`.
//!
//! Why: the pipelines must run WHILE the UI redraws, so they cannot run on the
//! thread that owns the terminal. This module is the boundary: it owns the
//! read connection the results view uses, spawns each run onto its own thread,
//! and hands progress back through the bus rather than through a channel of its
//! own.
//! What: [`WorkMode`], [`WorkerHandle`], and the [`run_work`] body.
//! Test: the pipelines `run_work` calls are unit-tested in
//! `tga::collect::correlate` and `tga::core::db::correlation`; the UI rules
//! that decide when it is called are in `super::tests::action_*`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use tga::collect::{correlate_commits, CollectionPipeline};
use tga::core::config::Config;
use tga::core::db::correlation::{correlation_counts, correlation_rows};
use tga::core::db::Database;
use tga::core::progress::{ProgressBus, ProgressEvent, Stage};

use super::state::{TuiState, RESULTS_LIMIT};

/// What a run should do.
///
/// Why: the correlate-only mode is what makes the TUI fully useful with no
/// credentials at all — it touches nothing but the local database.
/// What: `PullAndCorrelate` walks the selected repositories (and whatever
/// board/PR pulls the config already enables) and then correlates;
/// `CorrelateOnly` runs the deterministic link pass alone.
/// Test: `super::tests::action_run_requests_a_worker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkMode {
    /// Collect from the selected repositories, then correlate.
    PullAndCorrelate,
    /// Correlate only — no network, no credentials, no model.
    CorrelateOnly,
}

/// Owns the TUI's database reads and its background runs.
///
/// Why: keeping the connection and the spawn point together is what lets the
/// event loop stay pure glue.
/// What: a read connection for the results view, the database path each worker
/// re-opens, and the completion channel.
/// Test: exercised by running `tga tui`; the pieces it calls are unit-tested.
pub struct WorkerHandle {
    db: Database,
    db_path: PathBuf,
    done: Option<Receiver<String>>,
}

impl WorkerHandle {
    /// Build a handle around an already-open database.
    ///
    /// Why/What/Test: plain constructor; see the module doc.
    pub fn new(db: Database, db_path: PathBuf) -> Self {
        Self {
            db,
            db_path,
            done: None,
        }
    }

    /// Spawn a run in `mode` over the state's current selection.
    ///
    /// Why: the event loop must return to drawing immediately, so the run goes
    /// to its own thread and reports back through the bus and the done channel.
    ///
    /// It is a dedicated OS thread rather than a `tokio::spawn`, because
    /// [`CollectionPipeline::run`] holds a `&rusqlite::Connection` across await
    /// points and `Connection` is `Send` but not `Sync` — so its future is not
    /// `Send` and cannot be spawned onto the shared multi-threaded runtime. The
    /// thread builds its own current-thread runtime instead, which imposes no
    /// such bound.
    ///
    /// What: clones the narrowed config and the bus, spawns [`run_work`], and
    /// stores the receiver. A run already in flight is refused by the caller
    /// (see `event_loop::start`), not here.
    ///
    /// The thread is detached and never joined, so process exit cuts an
    /// in-flight fetch or per-week write off mid-flight. `event_loop::apply`
    /// therefore makes quitting during a run take a confirming second press
    /// rather than joining here (#5197) — see the `Action::Quit` arm for why
    /// joining was rejected.
    /// Test: exercised by running `tga tui`.
    pub fn start(&mut self, state: &TuiState, mode: WorkMode) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.done = Some(rx);
        let config = state.scoped_config();
        let bus = state.bus.clone();
        let db_path = self.db_path.clone();
        std::thread::spawn(move || {
            let summary = match run_work(config, db_path, mode, &bus) {
                Ok(s) => s,
                Err(e) => {
                    // Surface the failure in the progress pane too — a run that
                    // dies silently leaves every row spinning forever.
                    bus.emit(ProgressEvent::failed(
                        Stage::Correlate,
                        "run",
                        e.to_string(),
                    ));
                    format!("run failed: {e}")
                }
            };
            // A closed receiver means the operator quit mid-run; nothing to do.
            let _ = tx.send(summary);
        });
    }

    /// Take the finished run's summary, if one has arrived.
    ///
    /// Why: the loop polls rather than blocks so the UI keeps redrawing.
    /// What: `Some(summary)` once, then `None` until the next run. A worker
    /// that dropped its sender without sending (a panic in the task) reports
    /// as a lost run rather than leaving the UI stuck on "running…".
    /// Test: exercised by running `tga tui`.
    pub fn poll_finished(&mut self) -> Option<String> {
        let rx = self.done.as_ref()?;
        match rx.try_recv() {
            Ok(summary) => {
                self.done = None;
                Some(summary)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.done = None;
                Some("run ended without reporting a result".to_string())
            }
        }
    }

    /// Reload the correlation window into `state`.
    ///
    /// Why: the results view is a snapshot; it refreshes after a run and on
    /// `[g]` / a filter change.
    /// What: re-runs the counts and row queries and hands them to
    /// [`TuiState::set_results`]. A query error becomes a status-bar message
    /// rather than an exit — a broken read must not tear down the UI.
    /// Test: the queries are unit-tested in `tga::core::db::correlation`.
    pub fn reload_results(&mut self, state: &mut TuiState) {
        let counts = match correlation_counts(self.db.connection()) {
            Ok(c) => c,
            Err(e) => {
                state.set_message(format!("could not read correlation counts: {e}"));
                return;
            }
        };
        match correlation_rows(self.db.connection(), state.filter, RESULTS_LIMIT) {
            Ok(rows) => state.set_results(counts, rows),
            Err(e) => state.set_message(format!("could not read correlation rows: {e}")),
        }
    }
}

/// Run one pass and return its one-line summary.
///
/// Why: the worker body, kept out of [`WorkerHandle::start`] so the error path
/// is a plain `Result` rather than a nest of closures.
/// What: opens its own connection (the UI keeps its read one), optionally runs
/// the collection pipeline with `bus` attached on a private current-thread
/// tokio runtime, then runs the deterministic correlation pass. The runtime is
/// shut down with a zero deadline for the same reason `main` does it — idle
/// `reqwest` keep-alive tasks would otherwise hold the thread open for ~90 s.
///
/// Nothing here reads an LLM config or an API key; both modes are fully
/// deterministic, and `CorrelateOnly` additionally touches no network at all
/// (#5197).
///
/// # Errors
///
/// Propagates a database-open failure, a runtime-build failure, a collection
/// failure, or a correlation failure to the caller, which turns it into a
/// status-bar line.
///
/// `pub(super)` so `super::tests` can assert on the summary; nothing outside
/// this module calls it.
pub(super) fn run_work(
    config: Config,
    db_path: PathBuf,
    mode: WorkMode,
    bus: &ProgressBus,
) -> anyhow::Result<String> {
    let mut db = Database::open(&db_path)?;

    let collected = if mode == WorkMode::PullAndCorrelate {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let stats = runtime.block_on(
            CollectionPipeline::new(config)
                .with_progress(bus.clone())
                .run(&mut db),
        )?;
        runtime.shutdown_timeout(Duration::from_secs(0));
        // #5197: the error count travels with the commit count. A pipeline
        // whose per-repo failures only reached `stats.errors` used to reach
        // the TUI as a bare "collected N commit(s)", so `tga tui` had no
        // surface at all for a failure `tga collect` prints.
        Some((stats.commits_collected, stats.errors.len()))
    } else {
        None
    };

    let outcome = correlate_commits(db.connection_mut(), bus)?;

    Ok(match collected {
        Some((n, errors)) => format!(
            "collected {n} commit(s), {errors} error(s); {}",
            outcome.summary()
        ),
        None => format!("correlate only — {}", outcome.summary()),
    })
}
