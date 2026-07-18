//! Daemon-owned session registry: storage, per-session sequencing, ring
//! buffer, and attach/detach bookkeeping (#2054, #2055).
//!
//! Why: this is the object Axiom 4 describes — sessions live INSIDE the
//! daemon, not in a CLI-side tmux pane. `SessionRegistry` is the single
//! owner of every `Session`'s state, its bounded event history, and (#2055)
//! the per-session sequence counter that makes replay -> live gap detection
//! possible. Designed so persistence (Phase 2+, out of scope per §12) can be
//! added by swapping the storage backing this type without touching
//! `session::protocol`'s handlers or the wire protocol at all.
//! What: [`SessionRegistry`] holds `HashMap<String, SessionEntry>` behind a
//! `std::sync::Mutex` (never held across an `.await` — every method here is
//! synchronous; `attach` uses `tokio::spawn` to hand off, not to block).
//! Each entry carries the `Session` snapshot, a monotonic per-session `seq`
//! counter, a bounded ring buffer of recent
//! `crate::events::SessionEventEnvelope`s (§12.1.5: default 1000, oldest
//! dropped first), and a map of live attachments (`connection_id` ->
//! `oneshot::Sender<()>` used by `detach` to stop that connection's
//! forwarder task). [`Self::record`] is the ONE place that assigns `seq`
//! (under the same lock that pushes the ring buffer entry), so replay and
//! every subsequent live event share one gap-free, strictly increasing
//! sequence — see `crate::events`'s module docs for the full envelope
//! rationale. `attach` replays the ring buffer synchronously, then spawns a
//! task that subscribes to the existing process-global `crate::events` bus,
//! filters by `session_id`, and forwards matching envelopes into the
//! caller's `NotifySender` until detached or the channel closes (the
//! connection is gone).
//! Test: see `registry_tests` (sibling file, `#[cfg(test)]`).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::agent_loop::Transcript;
use crate::binding::ProjectBinding;
use crate::events::{ContextBudgetSnapshot, Event, IndexReadinessSnapshot, SessionEventEnvelope};
use crate::jsonrpc::{NotifySender, RpcError};
use crate::mode::HarnessMode;
use crate::perf::TokenUsage;
use crate::run_task::TurnRecord;

use super::model::{Session, SessionStatus};
use super::transcript::TranscriptRecord;

/// Default ring-buffer capacity per session (vision spec §12, 11.5).
///
/// Why: bounds per-session memory use; a freshly-attached client sees
/// "recent progress but not the full session history" per spec — the ring
/// buffer is the LIVE-event window, distinct from the full run transcript
/// `session.get_transcript` (#2058) exposes via `Self::get_transcript`.
/// Test: `registry_tests::ring_buffer_drops_oldest_when_full`.
pub const DEFAULT_RING_CAPACITY: usize = 1000;

/// One session's storage: the domain snapshot, its sequence counter, ring
/// buffer, the live attachments forwarding its events, and (#2056) its
/// background execution state + last completed run's outcome.
struct SessionEntry {
    session: Session,
    /// Last-assigned per-session sequence number; the next event gets
    /// `seq + 1`. Starts at `0` so the first recorded event is `seq = 1`.
    seq: u64,
    ring: VecDeque<SessionEventEnvelope>,
    /// `connection_id` -> the cancel sender for that connection's forwarder
    /// task. Re-attaching the same connection to the same session replaces
    /// (and thus cancels, via `Sender` drop) the previous forwarder.
    attachments: HashMap<Uuid, oneshot::Sender<()>>,
    /// #2056: set while a background task-execution is in flight; `None`
    /// when idle (never started, or already finished).
    execution: Option<Execution>,
    /// #2056: the run transcript populated once an execution finishes;
    /// exposed read-only via `session.get_transcript` (#2058,
    /// `Self::get_transcript`). Empty until the first execution completes.
    /// (#2344) CUMULATIVE across every run on this session — each
    /// `set_run_outcome` call APPENDS its turns rather than overwriting.
    transcript: Vec<TurnRecord>,
    /// #2056: aggregated token usage from every completed execution on this
    /// session. (#2344) CUMULATIVE — each `set_run_outcome` call adds its
    /// usage onto the running total rather than overwriting it.
    usage: TokenUsage,
    /// #2056: summed USD cost across every completed execution on this
    /// session (`None` if pricing was never available for any run — mirrors
    /// `run_task::RunReport::cost_usd`). (#2344) CUMULATIVE, same rule as
    /// `usage`.
    cost_usd: Option<f64>,
    /// (#2344) The PM's persistent, cumulative conversation `Transcript` —
    /// distinct from `transcript` above (which is the flat `TurnRecord`
    /// accounting view). `None` until the session's first `task.run` seeds
    /// it via [`SessionRegistry::begin_pm_transcript`]; briefly taken back
    /// out to `None` while a run is in flight (safe — `execution.is_some()`
    /// already guarantees at most one run at a time per session) and
    /// restored by [`SessionRegistry::store_pm_transcript`] once that run
    /// ends, regardless of outcome, so the NEXT run continues from exactly
    /// where this one left off.
    pm_transcript: Option<Transcript>,
    /// (#2345) The session's durable turn-recorder sink — `None` until the
    /// session's first `task.run` lazily constructs it via
    /// [`SessionRegistry::memory_sink_for`] (the project directory, needed to
    /// derive the palace id, is only known once a run starts). Built once and
    /// reused for the life of the session (see `memory_sink` module docs for
    /// why its background drain task must outlive any single run).
    memory_sink: Option<Arc<super::memory_sink::TurnMemorySink>>,
    /// (DOC-39 §5.6 Slice D) The most recently recorded `IndexReadiness`
    /// probe for this session — last-writer-wins, overwritten by every new
    /// `record_index_readiness` call. `None` until the session's first probe
    /// lands (or forever, for a session whose `task.run` never indexes —
    /// e.g. projectless). This is the storage `session.get_readiness` reads
    /// so a client that attaches AFTER the one-time `Event::IndexReadiness`
    /// fired can still retrieve the current state.
    readiness: Option<IndexReadinessSnapshot>,
    /// (issue #3015) The most recently recorded [`ContextBudgetSnapshot`] for
    /// this session — last-writer-wins, mirrors `readiness` above but for
    /// `record_context_budget`'s per-turn measurement. `None` until the
    /// session's first `Event::ContextBudget` fires (PM-only; a delegated
    /// sub-agent loop never emits one), so `session.get_context_budget` can
    /// distinguish "never recorded" from an all-zero snapshot.
    context_budget: Option<ContextBudgetSnapshot>,
    /// (DOC-39 §5.4) Live per-agent roster, in first-seen order — the
    /// AUTHORITATIVE state `session.get_agents` reads (see
    /// `session::registry::agents`'s module docs). Updated by [`Self::record`]
    /// (the SAME call every `record_tool_*` plumbing method already routes
    /// through) whenever the event carries `agent`/`agent_id`, so it is
    /// exactly as fresh as the ring buffer — but unlike `ring`, it is NEVER
    /// evicted by `ring_capacity`. A code-critic HIGH on the #2962 PR caught
    /// the bug this field exists to close: `session.get_agents` originally
    /// FOLDED the bounded ring buffer directly, so a long-running agent's
    /// `ToolStarted` could age out of the ring before any later attributed
    /// event for that same `agent_id` landed, making it vanish from the
    /// roster entirely — indistinguishable from "never spawned" while it may
    /// still genuinely be running. A `Vec` (not a `HashMap`) because the
    /// roster's first-seen ORDER is part of its contract and one session's
    /// distinct-agent cardinality is small, so `record`'s linear find-or-insert
    /// scan is cheap.
    agents: Vec<AgentRosterState>,
}

/// One agent's live roster state inside a [`SessionEntry`] (DOC-39 §5.4) —
/// see that field's own docs for why this exists separately from the ring
/// buffer. `session::registry::agents::get_agents` reads this directly (not
/// the ring) to build `session.get_agents`'s wire response.
///
/// Why: `agent_id` is stored inline (rather than as a `HashMap` key) so the
/// containing `Vec`'s order doubles as the roster's first-seen order with no
/// second parallel structure to keep in sync.
/// What: `running` is `true` from the last-seen `ToolStarted` for this
/// `agent_id` until the next `ToolFinished`/`ToolError`/telemetry event for
/// the SAME id flips it back to `false` — see
/// `session::registry::agents::tool_attribution`'s docs for why tool-dispatch
/// ordering makes "last event wins" well-defined without needing to
/// correlate `call_id`.
struct AgentRosterState {
    agent_id: String,
    name: String,
    running: bool,
}

impl SessionEntry {
    /// Fold one event into `self.agents` (DOC-39 §5.4) if it carries
    /// `agent`/`agent_id`; a no-op for every other `Event` variant.
    ///
    /// Why: the single update path for the always-retained roster — called
    /// from [`SessionRegistry::record`] under the SAME lock that pushes onto
    /// `ring`, so the two can never observe a different set of events.
    /// What: find-or-insert by `agent_id` (empty ids are skipped — nothing
    /// to key a roster row on), updating `name` (last-writer-wins) and
    /// `running` (`true` only for `ToolStarted`; every other attributed kind
    /// fires only after its tool call already completed — see
    /// `session::registry::agents`'s module docs for the tool-dispatch
    /// ordering that makes this well-defined).
    fn note_agent_activity(&mut self, event: &Event) {
        let Some((agent, agent_id, running)) = tool_attribution(event) else {
            return;
        };
        if agent_id.is_empty() {
            return;
        }
        match self.agents.iter_mut().find(|a| a.agent_id == agent_id) {
            Some(existing) => {
                existing.name = agent.to_string();
                existing.running = running;
            }
            None => self.agents.push(AgentRosterState {
                agent_id: agent_id.to_string(),
                name: agent.to_string(),
                running,
            }),
        }
    }
}

/// Extract `(agent, agent_id, is_running)` from the attribution events the
/// live roster (DOC-39 §5.4) cares about, or `None` for every other `Event`
/// variant.
///
/// Why: shared by [`SessionEntry::note_agent_activity`] (the write path) and
/// `session::registry::agents::get_agents`'s docs (which point back here) —
/// centralises which events count as roster-relevant activity.
/// What: `true` only for `ToolStarted`; every other attributed kind fires
/// only after its tool call already completed
/// (`ToolStarted` -> `ToolFinished`/`ToolError` -> optional telemetry).
fn tool_attribution(event: &Event) -> Option<(&str, &str, bool)> {
    match event {
        Event::ToolStarted {
            agent, agent_id, ..
        } => Some((agent, agent_id, true)),
        Event::ToolFinished {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        Event::ToolError {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        Event::SearchPerformed {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        Event::MemoryRecalled {
            agent, agent_id, ..
        } => Some((agent, agent_id, false)),
        _ => None,
    }
}

/// #2056: bookkeeping for one in-flight background task execution.
///
/// Why: `SessionRegistry` needs to (a) reject a second overlapping run for
/// the same session, (b) signal cooperative cancellation, and (c) — on
/// graceful daemon shutdown — wait, bounded, for the task to actually stop.
/// What: `cancel` is shared with every `AgentLoop`/`InProcessAgentRunner`
/// the execution drives (via `with_cancel_flag`); `handle` is the
/// `tokio::spawn` join handle, attached shortly after `begin_execution`
/// returns (see [`SessionRegistry::attach_execution_handle`]) since the
/// handle does not exist until the caller has actually spawned the task.
struct Execution {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// The daemon-owned session registry (Axiom 4).
///
/// Why: single source of truth for every session's state, in-memory only
/// (§12, M1) — see the module docs for the full rationale.
/// What: see method docs below; `new()` uses [`DEFAULT_RING_CAPACITY`],
/// `with_capacity` lets tests use a tiny buffer to exercise eviction cheaply.
/// Test: `registry_tests::*`.
pub struct SessionRegistry {
    sessions: Mutex<HashMap<String, SessionEntry>>,
    ring_capacity: usize,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    /// Construct an empty registry with the default ring-buffer capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_RING_CAPACITY)
    }

    /// Construct an empty registry with an explicit ring-buffer capacity.
    ///
    /// Why: unit tests need a small capacity to exercise eviction without
    /// pushing 1000 events.
    pub fn with_capacity(ring_capacity: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ring_capacity,
        }
    }

    /// `session.create`: register a brand-new session and publish its
    /// lifecycle-start events.
    ///
    /// Why: the sole entry point that mints a session id and puts a
    /// `Session` into the registry.
    /// What: builds a `Session` with a fresh UUIDv4 id and
    /// `status = Created`, inserts it (with a fresh `seq = 0` counter),
    /// publishes `Event::SessionStarted` (assigned `seq = 1`), then
    /// immediately transitions to `Running` (M1 has no queue ahead of real
    /// task execution — see `session::model` docs) which publishes
    /// `Event::SessionStatusChanged` (`seq = 2`). Returns the
    /// post-transition snapshot (so the caller sees `status: "running"`,
    /// not the momentary `"created"`).
    /// `binding` replaces what was an untyped `project: Option<String>` label.
    /// The label still exists on the wire (`Session.project`) but is now DERIVED
    /// from the binding via `ProjectBinding::label`, so the two can never
    /// disagree — see `Session`'s docs. `ProjectBinding::None` (projectless) is
    /// a fully supported binding, not a missing one.
    /// Test: `registry_tests::create_publishes_started_and_status_events`,
    /// `registry_tests::create_derives_project_label_from_binding`.
    pub fn create(&self, task: String, agent: Option<String>, binding: ProjectBinding) -> Session {
        let id = Uuid::new_v4().to_string();
        let session = Session {
            id: id.clone(),
            task,
            agent,
            project: binding.label(),
            binding,
            status: SessionStatus::Created,
            created_at: Utc::now(),
            mode: None,
        };
        {
            let mut sessions = self.lock();
            sessions.insert(
                id.clone(),
                SessionEntry {
                    session: session.clone(),
                    seq: 0,
                    ring: VecDeque::new(),
                    attachments: HashMap::new(),
                    execution: None,
                    transcript: Vec::new(),
                    usage: TokenUsage::default(),
                    cost_usd: None,
                    pm_transcript: None,
                    memory_sink: None,
                    readiness: None,
                    context_budget: None,
                    agents: Vec::new(),
                },
            );
        }
        self.record(
            &id,
            Event::SessionStarted {
                session_id: id.clone(),
                // The binding-derived label. Projectless yields `""`, exactly
                // as an absent `project` label already did before the binding
                // existed — no wire change for this event.
                project: session.project.clone().unwrap_or_default(),
            },
        );
        self.transition(&id, SessionStatus::Running);
        self.status(&id).unwrap_or(session)
    }

    /// `session.list`: snapshot every session currently in the registry.
    ///
    /// Why: the read side of the registry for operators/CLI/TUI to enumerate
    /// running sessions.
    /// What: clones every `Session` snapshot; order is unspecified (backed
    /// by a `HashMap`).
    /// Test: `registry_tests::list_returns_every_created_session`.
    pub fn list(&self) -> Vec<Session> {
        self.lock().values().map(|e| e.session.clone()).collect()
    }

    /// `session.status`: snapshot one session by id.
    ///
    /// Why: shared by `create`'s return value and the standalone
    /// `session.status` method.
    /// What: `Ok(Session)` if present, `Err(RpcError::session_not_found)`
    /// otherwise.
    /// Test: `registry_tests::status_returns_not_found_for_unknown_id`.
    pub fn status(&self, id: &str) -> Result<Session, RpcError> {
        self.lock()
            .get(id)
            .map(|e| e.session.clone())
            .ok_or_else(|| RpcError::session_not_found(id))
    }

    /// `session.send`: deliver input to a session and publish an observable
    /// event proving the daemon received it.
    ///
    /// Why: #2054/#2055 do not wire a real agent loop (that is `task.*`,
    /// #2056); publishing `Event::SessionInput` is what makes `session.send`
    /// observably reach a `session.attach`ed client NOW, and gives #2056 a
    /// concrete event to build real echo/ack behaviour on top of.
    /// What: errors with `session_not_found` if `id` is unknown; otherwise
    /// records+publishes `Event::SessionInput { session_id, input }`.
    /// Test: `registry_tests::send_publishes_input_event`,
    /// `registry_tests::send_unknown_session_errors`.
    pub fn send(&self, id: &str, input: &str) -> Result<(), RpcError> {
        if !self.lock().contains_key(id) {
            return Err(RpcError::session_not_found(id));
        }
        self.record(
            id,
            Event::SessionInput {
                session_id: id.to_string(),
                input: input.to_string(),
            },
        );
        Ok(())
    }

    /// `session.cancel`: send a cancellation signal and mark the session
    /// terminal.
    ///
    /// Why: vision spec §12 (11.6) Cancellation Semantics — cancellation is
    /// a status transition + event, not a deletion. #2054/#2055 have no
    /// in-flight agent loop to interrupt yet (that lands with #2056), so
    /// this is the full implementation available today: it marks the
    /// session `Cancelled` and publishes the terminal events; #2056 hooks
    /// the actual agent-loop interrupt onto the same status transition.
    /// What: idempotent — cancelling an already-terminal session is a no-op
    /// that returns the current snapshot rather than erroring (repeated
    /// `session.cancel` calls are safe). Errors with `session_not_found` if
    /// `id` is unknown. On a live session: publishes
    /// `Event::SessionStatusChanged` (`status: "cancelled"`), then the
    /// dedicated `Event::SessionCancelled` signal, then the generic terminal
    /// `Event::SessionDone` (`status: "cancelled"`) — matching the vision
    /// spec's `started/status-changed/input/cancelled/done` taxonomy — and
    /// returns the updated snapshot.
    /// Test: `registry_tests::cancel_transitions_to_cancelled_and_publishes`,
    /// `registry_tests::cancel_is_idempotent_on_terminal_session`,
    /// `registry_tests::cancel_unknown_session_errors`.
    pub fn cancel(&self, id: &str) -> Result<Session, RpcError> {
        let already_terminal = {
            let sessions = self.lock();
            let entry = sessions
                .get(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            entry.session.status.is_terminal()
        };
        if already_terminal {
            return self.status(id);
        }
        self.transition(id, SessionStatus::Cancelled);
        self.record(
            id,
            Event::SessionCancelled {
                session_id: id.to_string(),
            },
        );
        self.record(
            id,
            Event::SessionDone {
                session_id: id.to_string(),
                status: SessionStatus::Cancelled.as_str().to_string(),
            },
        );
        self.status(id)
    }

    /// Mark a session terminal (finished, failed, or cancelled while
    /// executing) and publish the terminal `Event::SessionDone` (#2056).
    ///
    /// Why: the background executor (`crate::task::executor`) needs to set
    /// the FINAL status once an agent loop returns, distinctly from the
    /// user-initiated `session.cancel` JSON-RPC path ([`Self::cancel`]),
    /// which ALSO emits a dedicated `Event::SessionCancelled` signal that
    /// only makes sense for an explicit user cancel request — not a natural
    /// finish or failure, and not a cancellation the EXECUTOR itself
    /// observed (that path already went through `request_cancel`, which
    /// does not transition status; the executor calls this afterward to
    /// actually land the transition once it has unwound).
    /// What: idempotent — a no-op (returns the current snapshot) if the
    /// session is already terminal, since a race between `request_cancel`'s
    /// caller and the executor noticing cancellation is possible but
    /// harmless. Errors with `session_not_found` if `id` is unknown.
    /// Otherwise transitions to `status` and publishes
    /// `Event::SessionDone { status }`.
    /// Test: `registry_tests::finish_transitions_and_publishes_session_done`,
    /// `registry_tests::finish_is_idempotent_on_terminal_session`,
    /// `registry_tests::finish_unknown_session_errors`.
    pub fn finish(&self, id: &str, status: SessionStatus) -> Result<Session, RpcError> {
        let already_terminal = {
            let sessions = self.lock();
            let entry = sessions
                .get(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            entry.session.status.is_terminal()
        };
        if !already_terminal {
            self.transition(id, status);
            self.record(
                id,
                Event::SessionDone {
                    session_id: id.to_string(),
                    status: status.as_str().to_string(),
                },
            );
        }
        self.status(id)
    }

    /// `session.attach`: replay recent history and start forwarding this
    /// session's live events to `notify`.
    ///
    /// Why: the streaming half of the session-attach protocol (vision spec
    /// §4.4/Axiom 4). Works identically regardless of transport — the
    /// difference between STDIO (long-lived `notify`, events interleaved on
    /// stdout) and HTTP (throwaway `notify`; the real live path is the `GET
    /// /sessions/{id}/events` SSE route) lives entirely in
    /// `crate::jsonrpc::ConnectionContext`'s docs and `crate::serve`'s
    /// transports, not here.
    /// What: errors with `session_not_found` if `id` is unknown. Otherwise
    /// takes a snapshot of the ring buffer (oldest-first, each entry a full
    /// [`SessionEventEnvelope`] with its `seq` intact) to return as the
    /// replay, registers `connection_id`'s cancel sender (replacing — and
    /// thus cancelling — any prior forwarder for the same
    /// `(session, connection)` pair), and spawns a task that subscribes to
    /// `crate::events::subscribe()`, filters by `session_id`, and forwards
    /// each match into `notify` as a JSON-RPC notification whose `params` IS
    /// the envelope (`{"jsonrpc":"2.0","method":"session.event","params":
    /// {"session_id":...,"seq":...,"at":...,"kind":...,"event":{...}}}`)
    /// until `detach` fires the cancel sender or `notify.send` fails (the
    /// connection is gone). Because every envelope — replayed or live —
    /// comes from the SAME per-session `seq` counter (`record`), a client
    /// that concatenates the replay array with the live notifications it
    /// receives afterward sees one gap-free, strictly increasing sequence.
    /// Test: `registry_tests::attach_returns_ring_buffer_replay`,
    /// `registry_tests::attach_forwards_live_events_until_detach`,
    /// `registry_tests::attach_replay_then_live_seq_is_contiguous`,
    /// `registry_tests::attach_unknown_session_errors`.
    pub fn attach(
        &self,
        id: &str,
        connection_id: Uuid,
        notify: NotifySender,
    ) -> Result<Vec<SessionEventEnvelope>, RpcError> {
        let replay = {
            let mut sessions = self.lock();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            let (cancel_tx, cancel_rx) = oneshot::channel();
            entry.attachments.insert(connection_id, cancel_tx);
            spawn_forwarder(id.to_string(), notify, cancel_rx);
            entry.ring.iter().cloned().collect::<Vec<_>>()
        };
        Ok(replay)
    }

    /// `session.detach`: stop forwarding this session's live events to the
    /// calling connection.
    ///
    /// Why: the other half of the attach protocol — releases the forwarder
    /// task started by `attach` for this `(session, connection)` pair.
    /// What: errors with `session_not_found` if `id` is unknown. If the
    /// connection has no live attachment (already detached, or never
    /// attached), this is a no-op success rather than an error — detach is
    /// idempotent. Otherwise fires the stored cancel sender, which stops the
    /// forwarder task on its next `select!` poll.
    /// Test: `registry_tests::detach_stops_the_forwarder`,
    /// `registry_tests::detach_without_prior_attach_is_a_noop`,
    /// `registry_tests::detach_unknown_session_errors`.
    pub fn detach(&self, id: &str, connection_id: Uuid) -> Result<(), RpcError> {
        let mut sessions = self.lock();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        if let Some(cancel_tx) = entry.attachments.remove(&connection_id) {
            let _ = cancel_tx.send(());
        }
        Ok(())
    }

    /// Snapshot the ring buffer for a session without attaching to it.
    ///
    /// Why: the HTTP SSE route (`crate::serve::http`) needs the same replay
    /// burst `attach` returns, but subscribes to the live bus itself (one
    /// `Stream` per SSE connection) rather than going through a
    /// `NotifySender` forwarder — so it calls this directly instead of
    /// `attach`.
    /// What: same replay semantics as `attach` (full envelopes, `seq`
    /// intact), no side effects (no forwarder spawned, no attachment
    /// recorded).
    /// Test: `registry_tests::replay_returns_ring_buffer_without_attaching`.
    pub fn replay(&self, id: &str) -> Result<Vec<SessionEventEnvelope>, RpcError> {
        self.lock()
            .get(id)
            .map(|e| e.ring.iter().cloned().collect())
            .ok_or_else(|| RpcError::session_not_found(id))
    }

    /// Begin tracking a new background task execution for `id` (#2056; #2344
    /// resumption semantics).
    ///
    /// Why: `task.run` (and `session.send` on an idle session) must not start
    /// a second overlapping run for the same session, and the returned flag
    /// is how the executor's `AgentLoop`(s) later observe cancellation.
    /// (#2344) A session that already `Finished` a previous run is NOT dead —
    /// per the infinite-sessions design, it is simply idle and ready for its
    /// next `task.run` to continue the SAME persistent conversation
    /// (`Self::begin_pm_transcript`). Only `Cancelled`/`Failed`/
    /// `DeadlineExceeded` remain genuinely terminal: those represent a broken
    /// run, and resuming one still requires a fresh session.
    /// What: errors with `session_not_found` if `id` is unknown,
    /// `invalid_argument` if the session is `Cancelled`/`Failed`/
    /// `DeadlineExceeded`, or already has an execution in flight — this
    /// SINGLE in-flight-execution guard is also this ticket's concurrency
    /// guard for `pm_transcript`: two overlapping `task.run` calls on one
    /// session can never both hold the transcript, because the second one is
    /// rejected here before either reaches `begin_pm_transcript`. On success,
    /// stores a fresh `Arc<AtomicBool>` cancel flag (initially `false`) with
    /// `handle: None` — the caller attaches the real `JoinHandle` via
    /// [`Self::attach_execution_handle`] immediately after spawning, since
    /// the handle cannot exist before that — and returns the flag. If the
    /// session was `Finished` (resuming), transitions it back to `Running`
    /// and publishes `Event::SessionStatusChanged` — matching the transition
    /// every other status change in this module publishes.
    /// Test: `registry_tests::begin_execution_rejects_second_overlapping_run`,
    /// `registry_tests::begin_execution_rejects_terminal_session`,
    /// `registry_tests::begin_execution_unknown_session_errors`,
    /// `registry_tests::begin_execution_resumes_a_finished_session`.
    pub fn begin_execution(&self, id: &str) -> Result<Arc<AtomicBool>, RpcError> {
        let (cancel, resumed) = {
            let mut sessions = self.lock();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            if matches!(
                entry.session.status,
                SessionStatus::Cancelled | SessionStatus::Failed | SessionStatus::DeadlineExceeded
            ) {
                return Err(RpcError::invalid_argument(format!(
                    "session {id} is already terminal"
                )));
            }
            if entry.execution.is_some() {
                return Err(RpcError::invalid_argument(format!(
                    "session {id} already has a task running"
                )));
            }
            let resumed = entry.session.status == SessionStatus::Finished;
            if resumed {
                entry.session.status = SessionStatus::Running;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            entry.execution = Some(Execution {
                cancel: Arc::clone(&cancel),
                handle: None,
            });
            (cancel, resumed)
        };
        if resumed {
            self.record(
                id,
                Event::SessionStatusChanged {
                    session_id: id.to_string(),
                    status: SessionStatus::Running.as_str().to_string(),
                },
            );
        }
        Ok(cancel)
    }

    /// Attach the real `JoinHandle` to `id`'s tracked execution (#2056).
    ///
    /// Why: the handle only exists once the caller has actually called
    /// `tokio::spawn`, which must happen after `begin_execution` returns (the
    /// spawned future needs the cancel flag `begin_execution` produces).
    /// What: best-effort — a no-op if `id` or its execution is already gone
    /// (e.g. the run finished astonishingly fast, or the session vanished).
    /// Test: `registry_tests::shutdown_executions_awaits_cancelled_tasks`
    /// (exercises this indirectly via a real spawn).
    pub fn attach_execution_handle(&self, id: &str, handle: JoinHandle<()>) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id)
            && let Some(execution) = entry.execution.as_mut()
        {
            execution.handle = Some(handle);
        }
    }

    /// Clear the execution-in-flight marker for `id` (#2056).
    ///
    /// Why: called by the executor once the background task has stopped
    /// (finished, failed, or cancelled) so a later `task.run`/`session.send`
    /// can start a new run against the same session.
    /// What: best-effort — a no-op if `id` is already gone.
    /// Test: `registry_tests::begin_execution_rejects_second_overlapping_run`
    /// (calls this between the two `begin_execution` attempts).
    pub fn finish_execution(&self, id: &str) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id) {
            entry.execution = None;
        }
    }

    /// Whether `id` currently has a background execution in flight (#2056).
    ///
    /// Why: `session::protocol::cancel` uses this to choose between
    /// cooperative cancellation ([`Self::request_cancel`]) and the original
    /// #2054 immediate-transition [`Self::cancel`].
    /// What: `false` for both "unknown session" and "known but idle" — callers
    /// that need to distinguish those already call another method first.
    /// Test: `registry_tests::request_cancel_*`.
    pub fn is_executing(&self, id: &str) -> bool {
        self.lock().get(id).is_some_and(|e| e.execution.is_some())
    }

    /// Request cooperative cancellation of `id`'s in-flight execution, if any
    /// (#2056).
    ///
    /// Why: `session.cancel` on a RUNNING session must signal the background
    /// task rather than force an immediate status transition — the task
    /// itself transitions to `Cancelled` once it actually observes the flag
    /// and unwinds (see `crate::task::executor`).
    /// What: `Err(session_not_found)` if `id` is unknown. `Ok(true)` if an
    /// execution was in flight and its flag was set; `Ok(false)` if the
    /// session exists but nothing is executing (the caller should fall back
    /// to [`Self::cancel`] for the immediate-transition path).
    /// Test: `registry_tests::request_cancel_sets_flag_when_executing`,
    /// `registry_tests::request_cancel_returns_false_when_idle`.
    pub fn request_cancel(&self, id: &str) -> Result<bool, RpcError> {
        let sessions = self.lock();
        let entry = sessions
            .get(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        match &entry.execution {
            Some(execution) => {
                execution.cancel.store(true, Ordering::Relaxed);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Persist a finished execution's transcript/usage/cost into the session
    /// (#2056; #2344 made CUMULATIVE across runs).
    ///
    /// Why: populates the storage `Self::get_transcript` (#2058) reads back
    /// over the wire. (#2344) A session's turns/usage/cost now accumulate
    /// across every `task.run` rather than the last run overwriting the
    /// previous one's — the whole point of a persistent session is that
    /// `session.get_transcript` shows the FULL conversation, not just its
    /// most recent slice.
    /// What: best-effort — a no-op if `id` is already gone (e.g. the daemon
    /// is shutting down mid-run). `transcript` is APPENDED onto the existing
    /// turns (not replacing them); `usage` is added field-wise via
    /// `TokenUsage::add` (the same accumulator `run_task::aggregate_usage_per_role`
    /// uses internally); `cost_usd` sums the two `Option<f64>`s the same way
    /// `TokenUsage::add` combines its own `cost_usd` field (`Some`+`Some` ->
    /// sum, either `Some` alone passes through, `None`+`None` stays `None`).
    /// Test: `registry_tests::set_run_outcome_stores_transcript_and_usage`,
    /// `registry_tests::set_run_outcome_accumulates_across_two_calls`.
    pub fn set_run_outcome(
        &self,
        id: &str,
        transcript: Vec<TurnRecord>,
        usage: TokenUsage,
        cost_usd: Option<f64>,
    ) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id) {
            entry.transcript.extend(transcript);
            entry.usage.add(&usage);
            entry.cost_usd = match (entry.cost_usd, cost_usd) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            };
        }
    }

    /// Retrieve this session's persistent PM conversation `Transcript`,
    /// seeding it on the session's FIRST `task.run` and appending `task` as a
    /// new user turn onto the existing history on every SUBSEQUENT one
    /// (#2344 — the infinite-sessions conversation-continuity entry point).
    ///
    /// Why: `task::executor::run_and_record` must hand the PM's `AgentLoop`
    /// the SAME growing `Transcript` every run drives via
    /// `AgentLoop::run_with_transcript`, rather than reseeding fresh each
    /// time (the pre-#2344 behaviour). Taking ownership OUT of the entry
    /// (rather than cloning) lets the caller drive the loop against it
    /// directly with no extra copy; briefly leaving `None` in storage while a
    /// run is active can never be observed by a second concurrent caller —
    /// `Self::begin_execution`'s single-in-flight-run guard already rejects a
    /// second `task.run` on the same session before it would ever reach this
    /// method (see that method's docs).
    /// What: `Err(session_not_found)` if `id` is unknown. If `pm_transcript`
    /// is `None` (the session has never run, or is between runs having been
    /// `take`n and not yet restored — the call-order guarantee above means
    /// the latter should never actually happen), seeds a fresh
    /// `Transcript::seed(system, task)`. If `Some`, takes the existing
    /// transcript out of storage and appends `task` as a new `user` turn via
    /// `Transcript::push_user` — `system` is IGNORED on this branch: the
    /// original seed's system message stays authoritative for the life of
    /// the session (a refresh mechanism, if `agent`/`mode` config changes
    /// between runs, is future work — not this ticket).
    /// Test: `registry_tests::begin_pm_transcript_seeds_on_first_call`,
    /// `registry_tests::begin_pm_transcript_appends_on_second_call`,
    /// `registry_tests::begin_pm_transcript_unknown_session_errors`.
    pub fn begin_pm_transcript(
        &self,
        id: &str,
        system: &str,
        task: &str,
    ) -> Result<Transcript, RpcError> {
        let mut sessions = self.lock();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        Ok(match entry.pm_transcript.take() {
            Some(mut existing) => {
                existing.push_user(task);
                existing
            }
            None => Transcript::seed(system, task),
        })
    }

    /// Persist the PM's conversation `Transcript` back onto the session once
    /// a run ends (#2344).
    ///
    /// Why: pairs with [`Self::begin_pm_transcript`] — MUST be called
    /// regardless of the run's outcome (success, failure, cancellation, or
    /// timeout) so the conversation up to the point of interruption survives
    /// for the NEXT `task.run` to continue from, rather than the session
    /// being left with no `pm_transcript` at all (which would silently
    /// re-seed from scratch on the next run instead of erroring).
    /// What: best-effort — a no-op if `id` is already gone (mirrors
    /// `Self::set_run_outcome`'s same best-effort framing).
    /// Test: `registry_tests::begin_pm_transcript_appends_on_second_call`
    /// (exercises both halves of the round trip).
    pub fn store_pm_transcript(&self, id: &str, transcript: Transcript) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id) {
            entry.pm_transcript = Some(transcript);
        }
    }

    /// `session.get_transcript`: fetch the stored run record for a session
    /// (#2058).
    ///
    /// Why: [`Self::set_run_outcome`] populates the storage; this is its read
    /// counterpart — the M1 cut-line's "inspect transcript" verb. A never-run
    /// session is a valid, empty transcript, not an error: `SessionEntry`'s
    /// `transcript`/`usage`/`cost_usd` fields already default to
    /// empty/zero/`None` at `create` time, so returning them unconditionally
    /// (once the session itself is confirmed to exist) is correct with no
    /// extra branching.
    /// What: `Err(session_not_found)` if `id` is unknown; otherwise a clone of
    /// whatever is currently stored, wrapped in a self-describing
    /// [`TranscriptRecord`]. Never recomputes cost or usage — exposes exactly
    /// what [`Self::set_run_outcome`] last stored. `compaction_events` (#2349)
    /// reads `entry.pm_transcript`'s own cumulative counter — `0` when the
    /// session has never run (`pm_transcript` still `None`).
    /// Test: `registry_tests::get_transcript_returns_stored_record`,
    /// `registry_tests::get_transcript_on_never_run_session_is_empty`,
    /// `registry_tests::get_transcript_unknown_session_errors`,
    /// `registry_tests::get_transcript_reports_compaction_events`,
    /// `registry_tests::get_transcript_round_trips_goal_state`.
    pub fn get_transcript(&self, id: &str) -> Result<TranscriptRecord, RpcError> {
        let sessions = self.lock();
        let entry = sessions
            .get(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        Ok(TranscriptRecord {
            session_id: id.to_string(),
            turns: entry.transcript.clone(),
            usage: entry.usage,
            cost_usd: entry.cost_usd,
            mode: entry.session.mode,
            compaction_events: entry
                .pm_transcript
                .as_ref()
                .map(Transcript::compaction_events)
                .unwrap_or(0),
            goals: goal_ops::goal_records(entry),
        })
    }

    /// `task.run`: persist the resolved `HarnessMode` onto the session
    /// (#2059).
    ///
    /// Why: `task::protocol::task_run` resolves the mode (env var >
    /// `task.run` param > `.claude/settings.json` > default) BEFORE spawning
    /// the execution; storing it here — rather than waiting for
    /// `set_run_outcome` at the end of the run — makes it queryable via
    /// `session.status`/`session.list` (`Session.mode`) and
    /// `session.get_transcript` (`TranscriptRecord.mode`, which reads the
    /// SAME field) as soon as `task.run` returns, not only after the run
    /// completes.
    /// What: `Err(session_not_found)` if `id` is unknown; otherwise sets
    /// `entry.session.mode = Some(mode)`. No event is published — mode is
    /// bookkeeping metadata, not a session-lifecycle transition in the
    /// #2055 event taxonomy.
    /// Test: `registry_tests::set_mode_stores_on_session`,
    /// `registry_tests::set_mode_unknown_session_errors`.
    pub fn set_mode(&self, id: &str, mode: HarnessMode) -> Result<(), RpcError> {
        let mut sessions = self.lock();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        entry.session.mode = Some(mode);
        Ok(())
    }

    /// Cooperatively cancel every in-flight execution and wait, bounded by
    /// `grace`, for them to actually stop (#2056 — graceful daemon shutdown).
    ///
    /// Why: without this, a `tokio::spawn`'d background execution would be
    /// abruptly dropped — never given a chance to unwind — the instant the
    /// process's tokio runtime shuts down, violating the connection-safe
    /// daemon-restart convention (issue #534) this extends from connections
    /// to long-running tasks.
    /// What: flips every tracked execution's cancel flag, takes ownership of
    /// each `JoinHandle`, then awaits all of them concurrently with an
    /// overall `grace` timeout. A handle that doesn't finish in time is left
    /// to be dropped when the process exits (best-effort, not a hard
    /// guarantee — matching the same best-effort framing as every other
    /// shutdown step in this codebase).
    /// Test: `registry_tests::shutdown_executions_awaits_cancelled_tasks`.
    pub async fn shutdown_executions(&self, grace: Duration) {
        let handles: Vec<JoinHandle<()>> = {
            let mut sessions = self.lock();
            sessions
                .values_mut()
                .filter_map(|entry| {
                    let execution = entry.execution.as_mut()?;
                    execution.cancel.store(true, Ordering::Relaxed);
                    execution.handle.take()
                })
                .collect()
        };
        if handles.is_empty() {
            return;
        }
        let _ = tokio::time::timeout(grace, futures_util::future::join_all(handles)).await;
    }

    /// Assign the next per-session `seq`, wrap `event` in a
    /// [`SessionEventEnvelope`], push it onto `id`'s ring buffer (evicting
    /// the oldest entry if full), and publish the envelope on the
    /// process-global bus.
    ///
    /// Why: every lifecycle/tool/log/progress/message event goes through
    /// this ONE function so `seq` assignment, the ring buffer, and the live
    /// bus can never drift out of sync with each other — the correctness
    /// property `attach_replay_then_live_seq_is_contiguous` verifies. It is
    /// ALSO the one place `SessionEntry::agents` (DOC-39 §5.4) is updated —
    /// the same critical section that pushes onto the capacity-bounded
    /// `ring`, so the always-retained roster and the ring can never observe
    /// a different event even transiently.
    /// What: a no-op (no envelope built, nothing published) if `id` is
    /// unknown — callers that need a hard error check existence first
    /// (`send`, `cancel`, every `record_*` plumbing method all do).
    fn record(&self, id: &str, event: Event) {
        let envelope = {
            let mut sessions = self.lock();
            let Some(entry) = sessions.get_mut(id) else {
                return;
            };
            entry.seq += 1;
            let envelope = SessionEventEnvelope::new(id.to_string(), entry.seq, Utc::now(), event);
            if entry.ring.len() >= self.ring_capacity {
                entry.ring.pop_front();
            }
            entry.ring.push_back(envelope.clone());
            entry.note_agent_activity(&envelope.event);
            envelope
        };
        crate::events::publish(envelope);
    }

    /// Update a session's status in place, then record+publish
    /// `Event::SessionStatusChanged`.
    fn transition(&self, id: &str, status: SessionStatus) {
        {
            let mut sessions = self.lock();
            if let Some(entry) = sessions.get_mut(id) {
                entry.session.status = status;
            }
        }
        self.record(
            id,
            Event::SessionStatusChanged {
                session_id: id.to_string(),
                status: status.as_str().to_string(),
            },
        );
    }

    /// Lock the session map, recovering from poisoning.
    ///
    /// Why: a panic inside another handler while holding this lock should
    /// not permanently wedge the whole registry for every other session —
    /// recovering the poisoned guard is safer than propagating a panic into
    /// unrelated requests.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionEntry>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Spawn the background task that forwards one session's live event
/// envelopes to a connection until cancelled or the connection is gone.
///
/// Why: split out of `attach` for readability; also gives `registry_tests`
/// a single well-named spawn site to reason about. Subscribing to the bus
/// SYNCHRONOUSLY here — before `tokio::spawn` schedules the forwarder body
/// — matters: if the subscription happened inside the spawned `async`
/// block instead, a caller that calls `send()` immediately after `attach()`
/// returns could publish its event before the spawned task ever gets polled
/// and subscribes, and `broadcast::Receiver`s only see events published
/// after `subscribe()` was called — the event would be silently missed.
/// What: subscribes to `crate::events::subscribe()` immediately, then
/// spawns a task that forwards envelopes whose `session_id` matches
/// `session_id` as a JSON-RPC notification via `notify` (`params` is the
/// envelope itself — `seq`/`at`/`kind`/`event` all present), skips
/// envelopes for other sessions and lag gaps, and exits on `cancel_rx`
/// firing (either a real detach or the sender being dropped) or
/// `notify.send` failing (connection gone).
/// Test: `registry_tests::attach_forwards_live_events_until_detach`.
fn spawn_forwarder(session_id: String, notify: NotifySender, cancel_rx: oneshot::Receiver<()>) {
    use tokio::sync::broadcast::error::RecvError;

    // Subscribe before spawning (see the Why above) so no event published
    // by the caller right after `attach()` returns can be missed.
    let mut events = crate::events::subscribe();

    tokio::spawn(async move {
        tokio::pin!(cancel_rx);
        loop {
            tokio::select! {
                biased;
                _ = &mut cancel_rx => break,
                received = events.recv() => match received {
                    Ok(envelope) if envelope.session_id == session_id => {
                        let notification = json!({
                            "jsonrpc": "2.0",
                            "method": "session.event",
                            "params": envelope,
                        });
                        if notify.send(notification).is_err() {
                            break;
                        }
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
            }
        }
    });
}

/// #2055 tool/log/progress/message event-recording plumbing, split out into
/// its own file (#2344) purely to keep this production file under the
/// 500-SLOC cap — these methods are still `impl SessionRegistry`, called
/// exactly the same way as if they lived here.
#[path = "registry_events.rs"]
mod events;

/// #2345 turn-recorder sink lazy-init/lookup, split out into its own file for
/// the same 500-SLOC-cap reason as `events` above.
#[path = "registry_memory_sink.rs"]
mod memory_sink_ext;

/// #2350 operator-facing goal-slot plumbing (`session.set_goal`/
/// `clear_goal`/`get_goals`), split out into its own file for the same
/// 500-SLOC-cap reason as `events`/`memory_sink_ext` above.
#[path = "registry_goals.rs"]
mod goal_ops;

/// `session.get_agents`'s live-roster fold (DOC-39 §5.4), split out into its
/// own file for the same 500-SLOC-cap reason as `events` above.
#[path = "registry_agents.rs"]
mod agents;

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
