//! Turn recorder (#2345): durable per-turn dual-write to trusty-memory.
//!
//! Why: epic #2343 (Infinite Sessions) requires every PM prompt/response
//! turn to be durably recorded in trusty-memory, independent of the
//! in-process `Transcript` (#2344), which only lives as long as the daemon
//! process. Without this, a daemon restart or crash loses the entire
//! conversation; #2348's `recall_session` tool also needs a semantic
//! recall surface over session history that a bare `chat_turn_append` record
//! alone would not give it (it stores the exact turn but is not
//! embedded/indexed for recall the way `memory_remember` is).
//! What: [`TurnMemorySink`] owns a bounded `tokio::sync::mpsc` queue and a
//! background drain task. [`TurnMemorySink::enqueue`] is the non-blocking
//! producer side, called from `task::executor::run_and_record` at each turn
//! boundary. The drain task calls BOTH `chat_turn_append` (the exact
//! chronological record) and `memory_remember` (tagged
//! `["session:<id>", "turn"]`, `force: true` — the semantic recall surface
//! for #2348) via the MCP `tools/call` envelope
//! (`crate::memory_envelope::call_tool_wrapped`; #2424 — trusty-memory's
//! direct-dispatch allowlist has no `chat_*` methods, so direct dispatch of
//! `chat_turn_append` fails `-32601` on every write), against a base URL
//! resolved ONCE at construction — never blocking or failing the calling
//! turn: any RPC failure is logged via `tracing::warn!` and dropped.
//! (#2424) Before the FIRST write, the drain task ensures the target palace
//! exists ([`ensure_palace`]) — `memory_remember` does NOT auto-create a
//! palace and fails `-32603 "palace metadata missing"` against a missing
//! one, which is exactly how the #2343 soak lost all 50 turns. The ensure
//! result is cached on success so steady state adds zero extra RPCs.
//! (#4638) That auto-create is gated on [`PalaceCreation`], decided from the
//! session's project root by `SessionRegistry::memory_sink_for`. The palace id
//! is DERIVED from that root, so an ephemeral root (a `tempfile::TempDir`)
//! yields a per-run-unique id and the ensure minted one permanent, unreadable
//! palace per run — 5,667 `t-tmp<random>` orphans in three weeks, 97.8% of
//! every palace on the machine, which made trusty-memory's O(n) full-registry
//! handlers (#4637) unusable. A palace is an expensive object (usearch index,
//! KG redb, drawer table, recall log), not a per-session scratch container:
//! the intended shape is ONE durable palace per PROJECT with each session
//! distinguished INSIDE it by `chat_turn_append`'s `session_id` and
//! `memory_remember`'s `session:<id>` tag, and that shape is what
//! [`PalaceCreation::Forbidden`] restores by making `palace_create`
//! unreachable from an ephemeral root.
//! (#2363) `memory_remember`'s dedup gate (jaro_winkler >0.92,
//! 5-min same-palace window) is documented as hostile to sequential
//! conversational turns, so every turn-recorder write passes `force: true`
//! to bypass it outright, along with the other content-QUALITY gates
//! (blocklist, short-content, noise pattern). Issue #2520 (two-tier
//! `force`): `force: true` no longer bypasses secret/credential detection —
//! this sink deliberately does NOT set the separate `allow_secret_like`
//! opt-in, so a turn whose raw LLM/tool-use content looks secret-shaped is
//! correctly REJECTED rather than persisted; this is a behavior change from
//! when `force` was a blanket bypass and is the intended safe default for an
//! automated writer. A `"status":"skipped"` response is still checked and
//! warned on as a belt-and-braces guard in case a gate skips the write.
//! [`TurnMemorySink::socket`]/[`TurnMemorySink::palace`] expose
//! this sink's already-resolved binding so #2348's `recall_session` tool can
//! target the SAME daemon/palace the turn recorder writes into, without a
//! second, independent resolution.
//! [`derive_palace_id_for_project`] mirrors
//! `trusty_common::catchup`'s (private) palace-derivation convention so a
//! session's turns land in the same palace its PM catch-up digest reads
//! from.
//! Test: `memory_sink::tests::*`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::memory_envelope::call_tool_wrapped;

/// Bounded mpsc queue capacity (#2345 scope: "~50").
///
/// Why: bounds memory use when trusty-memory is slow or unreachable for a
/// long stretch; a session realistically produces at most one turn per LLM
/// round trip, so 50 in-flight turns is generous slack before the overflow
/// policy below kicks in.
/// Test: `memory_sink::tests::enqueue_drops_newest_when_queue_full`.
pub const QUEUE_CAPACITY: usize = 50;

/// Which half of the dual-write degraded, as the retained per-session status
/// reports it (#2425).
///
/// Why: "the durable history is thinning" is not actionable on its own — an
/// operator needs to know whether the daemon is unreachable, the palace is
/// missing, a content gate rejected the write, or the local queue overflowed,
/// because those have different remedies. This is a CLOSED vocabulary rather
/// than the underlying error string precisely so it can be retained and
/// reported without carrying daemon-controlled text into the operator surface.
/// What: one variant per failure site in [`drain`]/[`write_turn`]/
/// [`TurnMemorySink::enqueue`]. Serialises `snake_case`.
/// Test: `memory_sink::tests::queue_full_and_closed_drain_are_immediate_failures`,
/// `registry_tests::memory_degradation_event_is_redacted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFailureCategory {
    /// The target palace could not be probed or created (#2424).
    PalaceEnsure,
    /// `chat_turn_append` returned an RPC error.
    ChatTurnAppend,
    /// `memory_remember` returned an RPC error.
    MemoryRemember,
    /// `memory_remember` returned `status=skipped` despite `force: true` (#2363).
    MemoryRememberSkipped,
    /// The bounded queue was full, so the newest turn was dropped.
    QueueFull,
    /// The drain task was gone, so the turn was dropped.
    DrainClosed,
}

/// The durability verdict for exactly one queued turn (#2425).
///
/// Why: durability is defined over QUEUED-TURN order, but outcomes do not
/// arrive in that order — a queue-full failure is reported synchronously by
/// [`TurnMemorySink::enqueue`] while accepted turns finish later in the
/// detached drain. `sequence` is what lets the consumer put them back in
/// logical order (see `memory_outcome_reconciler`).
/// What: `sequence` is assigned by `enqueue` from a monotonic per-sink
/// counter, so it is dense and gap-free across both reporting paths.
/// Test: `memory_sink::tests::queue_full_and_closed_drain_are_immediate_failures`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryTurnOutcome {
    Durable {
        sequence: u64,
    },
    Degraded {
        sequence: u64,
        category: MemoryFailureCategory,
        at: DateTime<Utc>,
    },
}

/// Sink-side hook that reports each turn's durability verdict (#2425).
///
/// Why: the sink must stay a leaf — it knows nothing about sessions or the
/// registry — but the retained status has to live on the session. An observer
/// inverts that dependency so `registry_memory_sink` installs the coupling and
/// the sink keeps its existing shape.
/// What: called once per queued turn, on whichever thread produced the verdict
/// (the caller's for a synchronous `enqueue` failure, the drain task's
/// otherwise). Implementations MUST NOT block or panic — see
/// `RegistryMemoryDurabilityObserver` for the fail-open contract.
/// Test: `memory_sink::tests::queue_full_and_closed_drain_are_immediate_failures`,
/// `memory_sink::tests::dropping_sink_releases_detached_drain_observer`.
pub(crate) trait MemoryDurabilityObserver: Send + Sync {
    fn observe(&self, outcome: MemoryTurnOutcome);
}

/// The observer a sink built without one gets — the pre-#2425 behaviour.
struct NoopMemoryDurabilityObserver;

impl MemoryDurabilityObserver for NoopMemoryDurabilityObserver {
    fn observe(&self, _outcome: MemoryTurnOutcome) {}
}

/// Whether a sink is entitled to bring its target palace into EXISTENCE
/// (#4638).
///
/// Why: the recorder's palace id is derived from the session's project root,
/// so an EPHEMERAL root yields an id that is unique per run — and `drain`'s
/// [`ensure_palace`] step then auto-created one permanent, unreadable palace
/// for every such run. That is how 5,667 `t-tmp<random>` orphans accumulated
/// in three weeks (97.8% of every palace on the machine), which in turn made
/// trusty-memory's O(n) full-registry handlers (#4637) unusable. A palace is
/// an expensive object (usearch vector index, KG redb, drawer table, recall
/// log), not a cheap per-session namespace, so the entitlement to mint one is
/// modelled explicitly rather than left implicit in "whoever writes first
/// wins". Making it a two-variant enum rather than a `bool` parameter means no
/// call site can pass the dangerous value by accident.
/// What: [`Self::Allowed`] reproduces the pre-#4638 behavior exactly (probe,
/// then `palace_create` on a miss — #2424); [`Self::Forbidden`] probes and
/// writes into a palace that already exists but never creates one, so a sink
/// carrying it can never increase the palace count.
/// Test: `memory_sink::tests::forbidden_creation_never_creates_a_palace`,
/// `memory_sink::tests::ensure_palace_creates_missing_palace_once`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalaceCreation {
    /// The project root is durable — auto-create the palace if missing (#2424).
    Allowed,
    /// The project root is ephemeral — never auto-create (#4638).
    Forbidden,
}

/// One user-prompt/assistant-response turn queued for durable dual-write.
#[derive(Debug, Clone)]
struct QueuedTurn {
    /// (#2425) Dense, monotonic position in this sink's queued-turn order.
    sequence: u64,
    session_id: String,
    prompt: String,
    response: String,
}

/// Async, fire-and-forget durable-write sink for one session's turns (#2345).
///
/// Why: see module docs.
/// What: `enqueue` never blocks the calling turn and never fails visibly —
/// see its docs for the overflow policy. The background drain task owns the
/// only receiver, so it keeps running for exactly as long as this sink (and
/// therefore the channel's sender half) stays alive — the session's
/// `SessionEntry` holds the constructed `Arc<TurnMemorySink>` for the
/// session's lifetime (built once, lazily, on the session's first
/// `task.run` — see `SessionRegistry::memory_sink_for`), so the drain task
/// naturally survives across every run on that session, not just one.
pub struct TurnMemorySink {
    tx: mpsc::Sender<QueuedTurn>,
    /// This sink's already-resolved trusty-memory base URL (#2348 reuse).
    socket: std::path::PathBuf,
    /// This sink's already-resolved palace id (#2348 reuse).
    palace: String,
    /// Whether this sink may bring `palace` into existence (#4638).
    creation: PalaceCreation,
    /// (#2425) Where each turn's durability verdict is reported.
    observer: Arc<dyn MemoryDurabilityObserver>,
    /// (#2425) Assigns each enqueued turn its `QueuedTurn::sequence`.
    next_sequence: AtomicU64,
}

impl TurnMemorySink {
    /// Construct a sink writing to `palace` at the given (already-resolved)
    /// `socket`, and spawn its background drain task with the default
    /// [`QUEUE_CAPACITY`].
    /// Test: `memory_sink::tests::enqueue_drain_happy_path`.
    pub fn new(socket: std::path::PathBuf, palace: String, creation: PalaceCreation) -> Self {
        Self::with_capacity(socket, palace, QUEUE_CAPACITY, creation)
    }

    /// Same as [`Self::new`], reporting each turn's durability verdict to
    /// `observer` (#2425).
    /// Test: `registry_tests::memory_durability_retains_counts_resets_streak_and_warns_at_one_and_three`.
    pub(crate) fn new_observed(
        socket: std::path::PathBuf,
        palace: String,
        creation: PalaceCreation,
        observer: Arc<dyn MemoryDurabilityObserver>,
    ) -> Self {
        Self::with_capacity_observed(socket, palace, QUEUE_CAPACITY, creation, observer)
    }

    /// Same as [`Self::new`] with an explicit queue capacity — tests use a
    /// tiny capacity to exercise the overflow policy cheaply.
    ///
    /// Why: `socket` is resolved ONCE by the caller (mirroring
    /// `catchup::pm_catchup_context`'s own
    /// `resolve_memory_socket_or_unreachable()` call) rather than
    /// re-resolved on every enqueued turn — the daemon's bound address does
    /// not change mid-session, and re-resolving on every turn would add
    /// discovery-file I/O to the hot drain path for no benefit. Tests inject
    /// a mock server's URL directly here instead of mutating the
    /// process-global `TRUSTY_MEMORY_SOCKET` env var (unsafe across parallel
    /// tests).
    /// What: spawns [`drain`] as a detached `tokio::spawn`ed task owning the
    /// receiver half of a `capacity`-bounded channel; returns the sink
    /// holding only the sender half.
    /// Test: `memory_sink::tests::enqueue_drops_newest_when_queue_full`.
    pub fn with_capacity(
        socket: std::path::PathBuf,
        palace: String,
        capacity: usize,
        creation: PalaceCreation,
    ) -> Self {
        Self::with_capacity_observed(
            socket,
            palace,
            capacity,
            creation,
            Arc::new(NoopMemoryDurabilityObserver),
        )
    }

    /// Same as [`Self::with_capacity`], reporting each turn's durability
    /// verdict to `observer` (#2425).
    ///
    /// Why: the drain task needs its own handle on the observer because it
    /// outlives no-one but the sink — the sink holds the sender half, so the
    /// task ends (and releases its `Arc`) when the sink drops.
    /// What: clones the `Arc` into the spawned [`drain`] and keeps one on the
    /// sink for [`Self::enqueue`]'s synchronous failure paths.
    /// Test: `memory_sink::tests::dropping_sink_releases_detached_drain_observer`.
    pub(crate) fn with_capacity_observed(
        socket: std::path::PathBuf,
        palace: String,
        capacity: usize,
        creation: PalaceCreation,
        observer: Arc<dyn MemoryDurabilityObserver>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        tokio::spawn(drain(
            socket.clone(),
            palace.clone(),
            creation,
            rx,
            Arc::clone(&observer),
        ));
        Self {
            tx,
            socket,
            palace,
            creation,
            observer,
            next_sequence: AtomicU64::new(1),
        }
    }

    /// Whether this sink may bring its palace into existence (#4638).
    ///
    /// Why: the #4638 bound is a property of the sink, so it must be
    /// observable to assert on directly rather than inferred from RPC traffic.
    /// What: returns the [`PalaceCreation`] fixed at construction.
    /// Test: `registry_tests::memory_sink_for_many_sessions_mint_at_most_one_palace`.
    pub fn palace_creation(&self) -> PalaceCreation {
        self.creation
    }

    /// This sink's already-resolved trusty-memory base URL.
    ///
    /// Why: #2348's `recall_session` tool must read from the SAME daemon the
    /// turn recorder writes into; re-deriving it independently would risk the
    /// two disagreeing (e.g. after a discovery-file update mid-session) and
    /// adds a redundant resolution for no benefit.
    /// What: Returns the URL passed to (or resolved by) [`Self::new`]/
    /// [`Self::with_capacity`] at construction — fixed for the sink's
    /// lifetime, mirroring `write_turn`'s own binding.
    /// Test: `memory_sink::tests::socket_and_palace_expose_construction_args`.
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// This sink's already-resolved palace id.
    ///
    /// Why: see [`Self::socket`] — the same reuse rationale applies to the
    /// palace binding.
    /// What: Returns the palace passed to [`Self::new`]/[`Self::with_capacity`]
    /// at construction.
    /// Test: `memory_sink::tests::socket_and_palace_expose_construction_args`.
    pub fn palace(&self) -> &str {
        &self.palace
    }

    /// Enqueue one turn for durable dual-write, never blocking the caller.
    ///
    /// Why: turn recording must NEVER stall or fail a running turn (#2345
    /// acceptance criteria) — a slow or wedged drain task must not back up
    /// into the agent loop.
    /// What: `try_send`s onto the bounded channel. Overflow policy: DROP THE
    /// NEWEST turn (this call's turn) rather than evicting an
    /// already-queued older one — the simplest policy `mpsc::Sender::
    /// try_send` supports directly (no peek/pop-front on the sender side
    /// without a different channel type), logged via `tracing::warn!` so an
    /// operator can see it happened. A closed receiver (the drain task
    /// panicked or was dropped) degrades the same way: logged, dropped, no
    /// error surfaced to the caller.
    /// Test: `memory_sink::tests::enqueue_drops_newest_when_queue_full`.
    pub fn enqueue(
        &self,
        session_id: impl Into<String>,
        prompt: impl Into<String>,
        response: impl Into<String>,
    ) {
        // #2425: the sequence is assigned here, on the ONE path every turn
        // takes, so the two reporting paths below share one dense ordering.
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        let turn = QueuedTurn {
            sequence,
            session_id: session_id.into(),
            prompt: prompt.into(),
            response: response.into(),
        };
        match self.tx.try_send(turn) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    failure_category = "queue_full",
                    tool = "turn_recorder",
                    status = "dropped",
                    "turn_recorder: queue full (capacity reached) — dropping newest turn"
                );
                self.observer.observe(MemoryTurnOutcome::Degraded {
                    sequence,
                    category: MemoryFailureCategory::QueueFull,
                    at: Utc::now(),
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!(
                    failure_category = "drain_closed",
                    tool = "turn_recorder",
                    status = "dropped",
                    "turn_recorder: drain task gone — dropping turn"
                );
                self.observer.observe(MemoryTurnOutcome::Degraded {
                    sequence,
                    category: MemoryFailureCategory::DrainClosed,
                    at: Utc::now(),
                });
            }
        }
    }
}

/// Background drain loop: pop turns off the channel and dual-write each one,
/// fail-open (see [`write_turn`]), ensuring the target palace exists before
/// the first write (#2424).
///
/// Why: the ensure lives HERE (drain-task-local state) rather than on the
/// sink struct so it needs no locking — the drain task is the only writer —
/// and so a failed ensure is naturally retried on the next turn (the flag is
/// only set on success), covering a daemon that comes up mid-session.
/// What: on success `palace_ensured` latches true and steady state adds
/// zero extra RPCs per turn; on failure under [`PalaceCreation::Allowed`] the
/// turn's writes are still attempted (fail-open — they carry their own
/// warnings).
///
/// (#4638) Under [`PalaceCreation::Forbidden`] a failed ensure means "this
/// palace does not exist and I am not entitled to create it", so the turn is
/// DROPPED instead of written: the two writes could not have landed anyway
/// (`memory_remember` fails `-32603 "palace metadata missing"` against an
/// absent palace), and skipping them keeps a temp-rooted session down to one
/// probe RPC per turn instead of three. The ensure is deliberately re-probed
/// rather than latched-off, so a Forbidden sink whose palace is created
/// out-of-band mid-session (or whose daemon was merely down for the first
/// probe) starts recording normally — "the daemon is unreachable" and "the
/// palace is absent" are not distinguishable from one failed probe, and only
/// the latter is permanent.
/// Test: `memory_sink::tests::ensure_palace_creates_missing_palace_once`,
/// `memory_sink::tests::ensure_palace_skips_create_when_palace_exists`,
/// `memory_sink::tests::forbidden_creation_never_creates_a_palace`,
/// `memory_sink::tests::forbidden_creation_still_writes_to_an_existing_palace`.
async fn drain(
    socket: std::path::PathBuf,
    palace: String,
    creation: PalaceCreation,
    mut rx: mpsc::Receiver<QueuedTurn>,
    observer: Arc<dyn MemoryDurabilityObserver>,
) {
    let mut palace_ensured = false;
    while let Some(turn) = rx.recv().await {
        if !palace_ensured {
            palace_ensured = ensure_palace(&socket, &palace, creation).await;
        }
        // #4638: no palace and no entitlement to make one — the writes below
        // are guaranteed to fail, so don't issue them.
        if !palace_ensured && creation == PalaceCreation::Forbidden {
            debug!(
                palace = %palace,
                session_id = %turn.session_id,
                "turn_recorder: dropping turn — palace absent and auto-create \
                 withheld for an ephemeral project root (#4638)"
            );
            observer.observe(MemoryTurnOutcome::Degraded {
                sequence: turn.sequence,
                category: MemoryFailureCategory::PalaceEnsure,
                at: Utc::now(),
            });
            continue;
        }
        observer.observe(write_turn(&socket, &palace, &turn).await);
    }
}

/// Ensure `palace` exists on the target daemon, creating it if missing
/// (#2424). Returns `true` when the palace is known to exist afterwards.
///
/// Why: `memory_remember` does NOT auto-create its palace — against a
/// missing one it fails `-32603 "palace metadata missing"`, which silently
/// killed the semantic half of every soak write. Probe-then-create (rather
/// than unconditional `palace_create`) because trusty-memory's
/// `handle_palace_create` OVERWRITES `palace.json` for an existing palace
/// (resetting `created_at`/`description`) — an existing project palace
/// shared with the PM catch-up digest must not have its metadata clobbered
/// on every session start.
/// What: `palace_info` via `tools/call`; on error (the daemon's signal for
/// "metadata missing" — or any other failure, in which case the create
/// simply fails too and we stay fail-open), `palace_create` with
/// `force: true` (the spec-001 documented bypass for app-managed palaces
/// whose slug does not match the DAEMON's cwd-derived project slug; a no-op
/// authz gate in default single-tenant mode) via `tools/call`. Never
/// propagates an error — failure is logged and reported as `false` so the
/// caller retries on the next turn.
///
/// (#4638) `creation` gates the CREATE half only — the probe always runs, so a
/// [`PalaceCreation::Forbidden`] sink still discovers and uses a palace that
/// already exists. Returning `false` without attempting a create is what makes
/// the bound structural: `palace_create` is unreachable from an ephemeral
/// project root, not merely unlikely.
/// Test: `memory_sink::tests::ensure_palace_creates_missing_palace_once`,
/// `memory_sink::tests::ensure_palace_skips_create_when_palace_exists`,
/// `memory_sink::tests::forbidden_creation_never_creates_a_palace`.
async fn ensure_palace(socket: &std::path::Path, palace: &str, creation: PalaceCreation) -> bool {
    if call_tool_wrapped(socket, "palace_info", json!({"palace": palace}))
        .await
        .is_ok()
    {
        return true;
    }
    if creation == PalaceCreation::Forbidden {
        return false;
    }
    let create_params = json!({
        "name": palace,
        "force": true,
        "description": "trusty-code session turn history (auto-created by the turn recorder)",
    });
    match call_tool_wrapped(socket, "palace_create", create_params).await {
        Ok(_) => {
            info!(palace = %palace, "turn_recorder: created missing palace (#2424)");
            true
        }
        // #2425: the daemon's error text is server-controlled and can be
        // arbitrarily long or carry a credential-shaped preview of the
        // rejected content, so the default warning names the failure
        // CATEGORY instead. See `write_turn` for the same treatment.
        Err(_error) => {
            warn!(
                failure_category = "palace_ensure",
                tool = "palace_create",
                status = "rpc_error",
                "turn_recorder: palace ensure failed (fail-open, will retry next turn)"
            );
            false
        }
    }
}

/// Dual-write one turn: `chat_turn_append` (the exact chronological record)
/// THEN `memory_remember` (the semantic recall surface, #2348).
///
/// Why: the exact and semantic representations are independent trusty-memory
/// endpoints; a mid-outage failure of one must not block the other, so each
/// call's error is handled separately rather than short-circuiting on the
/// first failure. (#2424) BOTH calls go through the MCP `tools/call`
/// envelope (`call_tool_wrapped`) — trusty-memory's direct-dispatch
/// allowlist (`TOOL_METHODS`) has no `chat_*` entries, so the previous
/// direct-method dispatch of `chat_turn_append` failed `-32601 Method not
/// found` on 100% of writes; `memory_remember` IS direct-dispatchable but
/// uses the same envelope anyway so both halves share one verified shape.
/// (#2363) `memory_remember` passes `force: true` because
/// its dedup gate (jaro_winkler >0.92, same-palace, 5-min window) is
/// documented as hostile to sequential conversational turns — near-duplicate
/// consecutive turns are the NORMAL case here, not noise; `force: true` also
/// bypasses the other content-QUALITY gates (blocklist, short-content, noise
/// pattern). Issue #2520: `force` does NOT bypass secret/credential
/// detection — a turn whose content looks secret-shaped comes back as an
/// `Err` from `call_tool_wrapped` (not a `"skipped"` status) and is logged
/// via the fail-open `Err(e)` arm below, same as any other RPC failure. A
/// `"status": "skipped"` response (from a gate `force` does NOT bypass, e.g.
/// a quality/blocklist gate that some OTHER path still applies) is still
/// checked and warned on so a silently thinned recall surface is at least
/// observable in logs — (#2424) `call_tool_wrapped` returns the PARSED inner
/// tool result, so the skipped detection sees the same shape it did under
/// direct dispatch.
/// What: never propagates an error — every failure is logged via
/// `tracing::warn!` and swallowed, matching
/// `resolve_memory_socket_or_unreachable`'s fail-open contract (mirrored
/// here, not reused directly, since `socket` is already resolved by the
/// caller of [`TurnMemorySink::new`]). (#2425) It RETURNS the turn's verdict
/// rather than propagating anything: `Degraded` with the first failed half's
/// category if either call failed, `Durable` otherwise. The default warnings
/// name that category and DELIBERATELY omit the daemon's own error text and
/// `reason` string, both of which are server-controlled and can quote rejected
/// content back — including the credential preview #2520's secret gate refused
/// to store. The daemon's own logs keep that text; this warning does not.
/// Test: `memory_sink::tests::enqueue_drain_happy_path`,
/// `memory_sink::tests::write_turn_is_fail_open_on_unreachable_daemon`,
/// `memory_sink::tests::write_turn_warns_on_skipped_status`,
/// `memory_sink::tests::partial_dual_write_counts_once_as_failed_turn`,
/// `memory_sink::tests::default_memory_warnings_redact_server_payloads_in_subprocess`.
async fn write_turn(
    socket: &std::path::Path,
    palace: &str,
    turn: &QueuedTurn,
) -> MemoryTurnOutcome {
    // #2425: the FIRST failure wins the reported category — a turn that lost
    // both halves is still exactly one degraded turn, not two.
    let mut failure = None;
    let append_params = json!({
        "palace": palace,
        "session_id": turn.session_id,
        "prompt": turn.prompt,
        "response": turn.response,
    });
    if let Err(_error) = call_tool_wrapped(socket, "chat_turn_append", append_params).await {
        failure = Some(MemoryFailureCategory::ChatTurnAppend);
        warn!(
            failure_category = "chat_turn_append",
            tool = "chat_turn_append",
            status = "rpc_error",
            "turn_recorder: chat_turn_append failed (fail-open)"
        );
    }

    let remember_params = json!({
        "palace": palace,
        "text": format!("User: {}\n\nAssistant: {}", turn.prompt, turn.response),
        "tags": [format!("session:{}", turn.session_id), "turn"],
        "force": true,
    });
    match call_tool_wrapped(socket, "memory_remember", remember_params).await {
        Ok(result) => {
            if result.get("status").and_then(|v| v.as_str()) == Some("skipped") {
                // #2425: `reason` is server-controlled and quotes the rejected
                // content back — for the secret-detection gate (#2520) that is
                // a preview of the very credential the gate refused to store.
                failure.get_or_insert(MemoryFailureCategory::MemoryRememberSkipped);
                warn!(
                    failure_category = "memory_remember_skipped",
                    tool = "memory_remember",
                    status = "skipped",
                    "turn_recorder: memory_remember returned status=skipped despite force=true \
                     (#2363) — session recall surface may be thinning"
                );
            }
        }
        Err(_error) => {
            failure.get_or_insert(MemoryFailureCategory::MemoryRemember);
            warn!(
                failure_category = "memory_remember",
                tool = "memory_remember",
                status = "rpc_error",
                "turn_recorder: memory_remember failed (fail-open)"
            );
        }
    }

    match failure {
        Some(category) => MemoryTurnOutcome::Degraded {
            sequence: turn.sequence,
            category,
            at: Utc::now(),
        },
        None => MemoryTurnOutcome::Durable {
            sequence: turn.sequence,
        },
    }
}

/// Resolve the palace id for a project directory (#2345).
///
/// Why: mirrors `trusty_common::catchup`'s own `derive_palace_id_for`
/// convention exactly, so a session's turns land in the SAME palace
/// `catchup::pm_catchup_context` reads its digest from — the PM's own catch-up
/// section and the turn recorder's writes must agree on "which palace is this
/// project."
///
/// This used to answer `"unknown-project"` on any failure. That literal is
/// SHARED: two projects that both fail resolution get the same id, and
/// `SessionRegistry::memory_sink_for` grants a durable project root
/// [`PalaceCreation::Allowed`], so the recorder auto-created one palace under
/// the placeholder and posted both projects' real prompts and responses into it
/// (#5811). While the only failure was "no identity at all" that was rare;
/// routing through `palace_resolve` added three pin-trust failures to the same
/// branch, so a typo in one committed pin was enough to reach it. The error is
/// returned instead, and the caller declines to record.
/// What: delegates to `trusty_common::palace_resolve::resolve_palace` — env
/// override, then the committed pin, then the git `owner/repo` slug, then the
/// `parent/dir` slug of the main worktree root.
/// Test: `memory_sink::tests::derive_palace_id_for_project_falls_back_to_dirname`,
/// `memory_sink::tests::malformed_pin_is_an_error_not_the_shared_placeholder`.
pub fn derive_palace_id_for_project(
    project_dir: &Path,
) -> Result<String, trusty_common::palace_resolve::PalaceResolveError> {
    // #5811: this probed the remote itself and called the PURE three-level
    // core, so a tcode session's turns landed in the derived palace even when
    // the project committed a pin naming a different one.
    trusty_common::palace_resolve::resolve_palace(project_dir).map(|resolution| resolution.id)
}

#[cfg(test)]
#[path = "memory_sink_tests.rs"]
mod tests;
