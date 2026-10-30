//! The LSP event producer: `lsp.<method>` Tool events on the trusty-events bus
//! (#9078, L4).
//!
//! Why: DOC-72 §4 makes this daemon a producer on the one event bus, beside
//! trusty-mpm, trusty-code and trusty-agents (DOC-73 §4.4, ADR-0065). An event
//! says that an LSP answer exists and what it cost, never what it is (§4.4,
//! D8), and the bus must never slow or fail a call (§4.5, D9).
//! What: [`LspEventPublisher`] owns one `PushClient` aimed at the bus socket.
//! [`LspCall::start`] pushes `Tool { Started }`; [`LspCall::finish`], or a
//! drop without it, pushes `Tool { Finished | Errored }` under the same
//! `call_id`, so a call never lacks a terminal event. Pushing only buffers
//! (bounded by count and bytes, overflow counted by `PushClient`).
//! [`LspEventPublisher::spawn_flush_task`] drains once per interval and
//! [`LspEventPublisher::final_flush`] once on exit, within
//! [`FINAL_FLUSH_BUDGET`]. [`attach_with`] wires a publisher at serve start
//! only while `ANALYZE_LSP` is on; with it off nothing here runs.
//! Test: the `events` module of `tests/lsp_rpc.rs` (stub bus), and the unit
//! tests in `lsp_events_tests.rs`.
//!
//! # Spec References
//! - [`SPEC-ANALYZELSP-04~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-04~draft)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trusty_common::control_bus::{
    ActionEvent, ActionMeta, Actor, CallPhase, EventId, HarnessEvent, HarnessPayload,
    HarnessSource, ObjectRef, ObjectType, PushClient, PushFlushOutcome,
};

use crate::lsp::LspError;
use crate::service::events::AnalyzerAppState;

/// `tool` of an `analyze.lsp_references` call.
pub const TOOL_REFERENCES: &str = "lsp.textDocument/references";
/// `tool` of an `analyze.lsp_diagnostics` call.
pub const TOOL_DIAGNOSTICS: &str = "lsp.publishDiagnostics";
/// `tool` of an `analyze.lsp_analysed_at` call.
pub const TOOL_ANALYSED_AT: &str = "lsp.analysed_at";

/// How often the flush task drains the buffer (DOC-72 §4.5).
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The most the exit path waits on the bus. Inside
/// [`crate::service::SHUTDOWN_FLUSH_TIMEOUT`], which also covers the unlink.
pub const FINAL_FLUSH_BUDGET: Duration = Duration::from_secs(1);

/// At most one "events dropped" warning per this window.
const DROP_WARN_EVERY: Duration = Duration::from_secs(60);

/// Metadata for one event, by name (`ActionMeta::attrs`).
pub type Attrs = Vec<(&'static str, Value)>;

/// The producer: one buffered `PushClient` plus the event builder.
///
/// Why: one instance per daemon, shared behind an `Arc` by every handler and
/// the flush task, as the `PushClient` contract asks.
/// What: `home` holds `$HOME` raw and canonical, so a path under it is shown
/// as `~/...` and no frame carries an absolute home path. `seq` is this
/// process's counter; the bus restamps it on arrival (DOC-73 §4.3).
/// Test: `lsp_call_pushes_started_and_finished_with_parent_id`.
pub struct LspEventPublisher {
    client: PushClient,
    home: Vec<PathBuf>,
    flush_interval: Duration,
    seq: AtomicU64,
}

impl LspEventPublisher {
    /// A publisher dialling `socket`, with `$HOME` from the environment.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            client: PushClient::new(socket),
            home: home_prefixes(dirs::home_dir()),
            flush_interval: DEFAULT_FLUSH_INTERVAL,
            seq: AtomicU64::new(0),
        }
    }

    /// Replace the home directory paths are shown relative to.
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home_prefixes(home);
        self
    }

    /// Replace the flush task's interval.
    pub fn with_flush_interval(mut self, every: Duration) -> Self {
        self.flush_interval = every;
        self
    }

    /// Drain the buffer to the bus once (the only call that does I/O).
    pub async fn flush(&self) -> PushFlushOutcome {
        self.client.flush().await
    }

    /// Events waiting for a flush.
    pub fn buffered_len(&self) -> usize {
        self.client.buffered_len()
    }

    /// Events lost to buffer overflow since start.
    pub fn dropped(&self) -> u64 {
        self.client.dropped()
    }

    /// Drain once per interval until aborted.
    ///
    /// Why: `send` only buffers; something must dial. The first tick waits a
    /// full interval, and a slow flush delays the next tick rather than
    /// stacking (§4.5). It holds no connection between flushes, so it never
    /// keeps the daemon from idling out.
    /// Test: `the_flush_task_delivers_while_serving`.
    pub fn spawn_flush_task(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let every = me.flush_interval;
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut health = FlushHealth::default();
            loop {
                tick.tick().await;
                let outcome = me.client.flush().await;
                health.report(&outcome, me.client.dropped(), Instant::now());
            }
        })
    }

    /// The exit-path flush, bounded by [`FINAL_FLUSH_BUDGET`].
    ///
    /// Test: `final_flush_runs_on_idle_exit_and_on_shutdown`.
    pub async fn final_flush(&self) {
        match tokio::time::timeout(FINAL_FLUSH_BUDGET, self.client.flush()).await {
            Ok(PushFlushOutcome { error: None, sent }) => {
                tracing::debug!(sent, "lsp events: final flush delivered");
            }
            Ok(PushFlushOutcome {
                error: Some(e),
                sent,
            }) => tracing::warn!(
                sent,
                undelivered = self.client.buffered_len(),
                error = %e,
                "lsp events: final flush stopped; the rest are lost"
            ),
            // The cut-off write re-queued its frame, so `undelivered` counts it.
            Err(_) => tracing::warn!(
                undelivered = self.client.buffered_len(),
                "lsp events: final flush outlived its budget; the rest are lost"
            ),
        }
    }

    /// `path` for an event: `~/...` under `$HOME`, otherwise as given.
    fn shown(&self, path: &Path) -> String {
        for home in &self.home {
            if let Ok(rest) = path.strip_prefix(home) {
                return if rest.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", rest.display())
                };
            }
        }
        path.display().to_string()
    }

    /// Build and buffer one `Tool` event; returns its id.
    fn push(&self, step: Step<'_>, phase: CallPhase, attrs: BTreeMap<String, Value>) -> EventId {
        let id = EventId::new();
        let at = chrono::Utc::now();
        let meta = ActionMeta {
            id,
            at,
            source: HarnessSource::Analyze,
            session: None,
            parent_id: step.parent,
            actor: Actor::System,
            objects: step.objects.to_vec(),
            attrs,
            schema_version: 1,
        };
        self.client.send(HarnessEvent {
            source: HarnessSource::Analyze,
            session: None,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            at,
            payload: HarnessPayload::Action(ActionEvent::Tool {
                meta,
                phase,
                tool: step.tool.to_string(),
                call_id: step.call_id.to_string(),
            }),
            id,
            parent_id: step.parent,
        });
        id
    }
}

/// The parts of a `Tool` event that vary per push.
struct Step<'a> {
    tool: &'a str,
    call_id: &'a str,
    parent: Option<EventId>,
    objects: &'a [ObjectRef],
}

/// `home`, plus its canonical form when that differs (macOS `/var` vs
/// `/private/var`), so either spelling of a path collapses to `~`.
fn home_prefixes(home: Option<PathBuf>) -> Vec<PathBuf> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut out = vec![home.clone()];
    if let Ok(canonical) = std::fs::canonicalize(&home) {
        if canonical != home {
            out.push(canonical);
        }
    }
    out
}

/// One `analyze.lsp_*` call between its Started and terminal events.
///
/// Why: DOC-72 §4.2 — a call never ends without a terminal event, including
/// a call whose future is dropped mid-flight (the client hung up).
/// What: a no-op when the flag is off or no publisher is attached. Otherwise
/// `start` pushes Started; `finish` pushes Finished or Errored with
/// `outcome`, `duration_ms` and the stamps; `Drop` without `finish` pushes
/// Errored with outcome `cancelled`. The terminal event's `parent_id` is the
/// Started event, which is how the tree closes the node.
/// Test: `lsp_call_pushes_started_and_finished_with_parent_id`,
/// `errored_terminal_event_on_server_crash`,
/// `a_dropped_call_ends_errored_cancelled`.
pub struct LspCall {
    live: Option<Live>,
}

struct Live {
    publisher: Arc<LspEventPublisher>,
    tool: &'static str,
    call_id: String,
    started_id: EventId,
    objects: Vec<ObjectRef>,
    started: Instant,
    attrs: BTreeMap<String, Value>,
}

impl LspCall {
    /// Push Started for `path` if the capability is on and a bus is attached.
    pub fn start(
        state: &AnalyzerAppState,
        tool: &'static str,
        path: &Path,
        parent: Option<EventId>,
        attrs: Attrs,
    ) -> Self {
        let Some(publisher) = state.lsp_events.as_ref().filter(|_| state.lsp.is_enabled()) else {
            return Self { live: None };
        };
        Self::start_on(publisher, tool, path, parent, attrs)
    }

    /// [`LspCall::start`] against `publisher`, with no flag check.
    pub(crate) fn start_on(
        publisher: &Arc<LspEventPublisher>,
        tool: &'static str,
        path: &Path,
        parent: Option<EventId>,
        attrs: Attrs,
    ) -> Self {
        let call_id = EventId::new().to_string();
        let objects = vec![ObjectRef {
            object_type: ObjectType::File,
            id: publisher.shown(path),
            label: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }];
        let mut first: BTreeMap<String, Value> =
            attrs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        // 1.9.0 serves Rust only (DOC-72 §12, D2).
        first.insert("language".into(), json!("rust"));
        let step = Step {
            tool,
            call_id: &call_id,
            parent,
            objects: &objects,
        };
        let started_id = publisher.push(step, CallPhase::Started, first);
        Self {
            live: Some(Live {
                publisher: Arc::clone(publisher),
                tool,
                call_id,
                started_id,
                objects,
                started: Instant::now(),
                attrs: BTreeMap::new(),
            }),
        }
    }

    /// Record the workspace root the call resolved, shown home-relative.
    pub fn workspace(&mut self, root: &Path) {
        if let Some(live) = self.live.as_mut() {
            let shown = live.publisher.shown(root);
            live.attrs.insert("workspace".into(), json!(shown));
        }
    }

    /// Push the terminal event for `result`; `stamps` names the success
    /// metadata (counts, document version, snapshot). An error contributes
    /// only its stable code, never its message, which can quote server
    /// stderr (D8).
    pub fn finish<T>(self, result: &Result<T, LspError>, stamps: impl FnOnce(&T) -> Attrs) {
        self.finish_as(result, |v| ("ok", stamps(v)));
    }

    /// [`LspCall::finish`] for a success that is not always `ok`.
    ///
    /// Why: #10087 — a barrier that timed out is an answer (DOC-72 §3), so the
    /// call Finishes, but a subscriber must not read it as ready.
    /// What: `classify` names the `outcome` and the stamps for an `Ok`; an
    /// `Err` still ends Errored with its code.
    /// Test: `analysed_at_timeout_finishes_as_timed_out_not_ok`.
    pub fn finish_as<T>(
        mut self,
        result: &Result<T, LspError>,
        classify: impl FnOnce(&T) -> (&'static str, Attrs),
    ) {
        let (phase, outcome, extra) = match result {
            Ok(v) => {
                let (outcome, extra) = classify(v);
                (CallPhase::Finished, outcome, extra)
            }
            Err(e) => (CallPhase::Errored, e.code(), Vec::new()),
        };
        self.end(phase, outcome, extra);
    }

    fn end(&mut self, phase: CallPhase, outcome: &str, extra: Attrs) {
        let Some(mut live) = self.live.take() else {
            return;
        };
        let elapsed = u64::try_from(live.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        live.attrs.insert("outcome".into(), json!(outcome));
        live.attrs.insert("duration_ms".into(), json!(elapsed));
        for (k, v) in extra {
            live.attrs.insert(k.to_string(), v);
        }
        let step = Step {
            tool: live.tool,
            call_id: &live.call_id,
            parent: Some(live.started_id),
            objects: &live.objects,
        };
        live.publisher.push(step, phase, live.attrs);
    }
}

impl Drop for LspCall {
    fn drop(&mut self) {
        self.end(CallPhase::Errored, "cancelled", Vec::new());
    }
}

/// Rate-limits what the flush task says about the bus.
///
/// Why: a bus that is down for an hour must cost one warning, not 3,600, and
/// overflow must be visible without flooding the log (§4.5).
/// What: warns once when the bus becomes unreachable and logs once when it
/// recovers; warns about a grown drop count at most once per
/// [`DROP_WARN_EVERY`]. `report` returns what it logged, for tests.
/// Test: `flush_health_warns_once_per_outage_and_rate_limits_drops`.
#[derive(Default)]
pub(crate) struct FlushHealth {
    down: bool,
    dropped_seen: u64,
    last_drop_warn: Option<Instant>,
}

/// What [`FlushHealth::report`] logged.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    Down,
    Recovered,
    Dropped(u64),
}

impl FlushHealth {
    pub(crate) fn report(
        &mut self,
        outcome: &PushFlushOutcome,
        dropped: u64,
        now: Instant,
    ) -> Vec<Notice> {
        let mut said = Vec::new();
        match (&outcome.error, self.down) {
            (Some(e), false) => {
                tracing::warn!(error = %e, "lsp events: trusty-events unreachable; buffering (bounded)");
                self.down = true;
                said.push(Notice::Down);
            }
            (None, true) if outcome.sent > 0 => {
                tracing::info!("lsp events: trusty-events reachable again");
                self.down = false;
                said.push(Notice::Recovered);
            }
            _ => {}
        }
        let due = self
            .last_drop_warn
            .is_none_or(|t| now.duration_since(t) >= DROP_WARN_EVERY);
        if dropped > self.dropped_seen && due {
            tracing::warn!(
                dropped,
                "lsp events: buffer overflowed; oldest events dropped"
            );
            self.dropped_seen = dropped;
            self.last_drop_warn = Some(now);
            said.push(Notice::Dropped(dropped));
        }
        said
    }
}

/// Attach a publisher aimed at the bus to `state`, if the capability is on.
///
/// Why: with `ANALYZE_LSP` off the daemon must behave exactly as before, so
/// the socket is not even resolved (resolving creates a data directory). A
/// bus that cannot be resolved is a warning, never a failed start (§4.5).
/// What: returns `state` unchanged when the flag is off or `resolve` fails;
/// otherwise with a publisher on the resolved path.
/// Test: `flag_off_emits_no_events`, `an_unresolvable_bus_still_serves`.
pub fn attach_with(
    state: AnalyzerAppState,
    resolve: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> AnalyzerAppState {
    if !state.lsp.is_enabled() {
        return state;
    }
    match resolve() {
        Ok(socket) => state.with_lsp_events(Arc::new(LspEventPublisher::new(socket))),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "lsp events: no trusty-events socket; events are off for this run");
            state
        }
    }
}

/// [`attach_with`] on `control_bus::events_socket_path`, for the serve entry
/// points.
pub fn attach(state: AnalyzerAppState) -> AnalyzerAppState {
    attach_with(state, trusty_common::control_bus::events_socket_path)
}

#[cfg(test)]
#[path = "lsp_events_tests.rs"]
mod tests;
