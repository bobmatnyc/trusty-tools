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
use std::sync::Mutex;

use chrono::Utc;
use serde_json::json;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::events::{Event, SessionEventEnvelope};
use crate::jsonrpc::{NotifySender, RpcError};

use super::model::{Session, SessionStatus};

/// Default ring-buffer capacity per session (vision spec §12, 11.5).
///
/// Why: bounds per-session memory use; a freshly-attached client sees
/// "recent progress but not the full session history" per spec, with full
/// history reserved for a future `session.get_transcript` (out of scope for
/// #2054/#2055 — not in either issue's method list).
/// Test: `registry_tests::ring_buffer_drops_oldest_when_full`.
pub const DEFAULT_RING_CAPACITY: usize = 1000;

/// One session's storage: the domain snapshot, its sequence counter, ring
/// buffer, and the live attachments forwarding its events.
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
    /// Test: `registry_tests::create_publishes_started_and_status_events`.
    pub fn create(&self, task: String, agent: Option<String>, project: Option<String>) -> Session {
        let id = Uuid::new_v4().to_string();
        let session = Session {
            id: id.clone(),
            task,
            agent,
            project: project.clone(),
            status: SessionStatus::Created,
            created_at: Utc::now(),
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
                },
            );
        }
        self.record(
            &id,
            Event::SessionStarted {
                session_id: id.clone(),
                project: project.unwrap_or_default(),
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

    /// Record a tool invocation starting (#2055 emission plumbing for
    /// #2056's agent loop).
    ///
    /// Why: gives the future agent loop a stable, already-wired call for
    /// `ToolStarted` without it needing to touch the ring buffer, sequencing,
    /// or bus directly.
    /// What: errors with `session_not_found` if `id` is unknown; `args_preview`
    /// is truncated via `crate::events::preview` before emission so a large
    /// argument payload can't blow the ring buffer / wire size.
    /// Test: `registry_tests::record_tool_started_publishes_event`.
    pub fn record_tool_started(
        &self,
        id: &str,
        tool: &str,
        call_id: &str,
        args: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolStarted {
                session_id: id.to_string(),
                tool: tool.to_string(),
                call_id: call_id.to_string(),
                args_preview: crate::events::preview(args, 500),
            },
        );
        Ok(())
    }

    /// Record a tool invocation finishing (#2055 emission plumbing for
    /// #2056's agent loop). See [`Self::record_tool_started`].
    /// Test: `registry_tests::record_tool_finished_publishes_event`.
    pub fn record_tool_finished(
        &self,
        id: &str,
        tool: &str,
        call_id: &str,
        success: bool,
        result: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolFinished {
                session_id: id.to_string(),
                tool: tool.to_string(),
                call_id: call_id.to_string(),
                success,
                result_preview: crate::events::preview(result, 500),
            },
        );
        Ok(())
    }

    /// Record a tool invocation raising an exceptional error (#2055 emission
    /// plumbing for #2056's agent loop). See [`Self::record_tool_started`].
    /// Test: `registry_tests::record_tool_error_publishes_event`.
    pub fn record_tool_error(
        &self,
        id: &str,
        tool: &str,
        call_id: &str,
        error: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolError {
                session_id: id.to_string(),
                tool: tool.to_string(),
                call_id: call_id.to_string(),
                error: error.to_string(),
            },
        );
        Ok(())
    }

    /// Record a session-scoped diagnostic log line (#2055 emission plumbing).
    /// Test: `registry_tests::record_log_publishes_event`.
    pub fn record_log(&self, id: &str, level: &str, message: &str) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::Log {
                session_id: id.to_string(),
                level: level.to_string(),
                message: message.to_string(),
            },
        );
        Ok(())
    }

    /// Record a coarse progress update (#2055 emission plumbing).
    /// Test: `registry_tests::record_progress_publishes_event`.
    pub fn record_progress(
        &self,
        id: &str,
        message: &str,
        percent: Option<f32>,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::Progress {
                session_id: id.to_string(),
                message: message.to_string(),
                percent,
            },
        );
        Ok(())
    }

    /// Record a generic freeform session message (#2055 emission plumbing).
    /// Test: `registry_tests::record_message_publishes_event`.
    pub fn record_message(&self, id: &str, text: &str) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::Message {
                session_id: id.to_string(),
                text: text.to_string(),
            },
        );
        Ok(())
    }

    /// Return `Ok(())` if `id` exists, `Err(session_not_found)` otherwise.
    ///
    /// Why: every `record_*` plumbing method needs the same existence guard
    /// before recording; centralising it keeps each one a two-line body.
    fn ensure_exists(&self, id: &str) -> Result<(), RpcError> {
        if self.lock().contains_key(id) {
            Ok(())
        } else {
            Err(RpcError::session_not_found(id))
        }
    }

    /// Assign the next per-session `seq`, wrap `event` in a
    /// [`SessionEventEnvelope`], push it onto `id`'s ring buffer (evicting
    /// the oldest entry if full), and publish the envelope on the
    /// process-global bus.
    ///
    /// Why: every lifecycle/tool/log/progress/message event goes through
    /// this ONE function so `seq` assignment, the ring buffer, and the live
    /// bus can never drift out of sync with each other — the correctness
    /// property `attach_replay_then_live_seq_is_contiguous` verifies.
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

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
