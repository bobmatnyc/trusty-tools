//! Main-thread tickets for the Architect's `set` and local `copy` (#10459
//! PR1, DOC-86 §2).
//!
//! Why: P1's launch-record exemption alone covers the Architect's whole
//! `claude` tree — subagents, MCP servers, background children. DOC-86
//! narrows it to one call the Architect's main-thread PreToolUse hook
//! announced: the hook mints a ticket bound to that call, and a denied key
//! opens only to a caller holding a redeemed ticket AND a valid launch
//! record (Q4). Minting is off in this release (`mint_enabled`), so the
//! behaviour equals the agent rules.
//! What: [`TicketStore`], daemon memory only — no file, environment or argv
//! carrier, and a restart drops every ticket. A ticket is one-use, lives
//! [`TICKET_TTL`], and binds the session, prompt and tool-use ids, the
//! redeemer's argv hash, the `claude` (pid, start time), the op, the project
//! and the key. Every read happens under one lock ([`TicketStore::locked`]);
//! the method bodies in `ticket_methods` mint, void, query and redeem through
//! it and write each record before the change it describes takes effect.
//! [`TicketAudit`] is the record's `ticket` object; it never holds a value.
//! Known limit: same-uid code can forge a mint, or redeem in the window; a
//! redeemed ticket proves a mint happened, not who caused it. See #10459,
//! DOC-86 §1.
//! Test: `ticket_tests.rs` beside `wire_tests.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ancestry::StartTime;
use super::grant::{Clock, SystemClock};
use crate::api::SecretKey;

/// How long a minted ticket may be redeemed (DOC-86 §2).
pub const TICKET_TTL: Duration = Duration::from_secs(60);

/// The most live tickets the store holds; one per session is the norm.
pub(crate) const MAX_LIVE_TICKETS: usize = 64;

/// What a ticket opens. PR1 knows the secrets operation only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TicketKind {
    /// One `tm secrets set` or local `tm secrets copy`.
    SecretsOp,
}

/// The method a `secrets_op` ticket is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TicketOp {
    /// `secrets.set`.
    Set,
    /// A local `secrets.copy` of one key.
    Copy,
}

/// What happened to a ticket, on its audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TicketEvent {
    /// Minted and stored.
    Mint,
    /// Consumed by the `set` or `copy` it was minted for.
    Redeem,
    /// Removed unredeemed; `void_cause` says why.
    Void,
    /// A mint the server refused; `reason_arm` says which check.
    Refuse,
    /// An event a newer server records. Never written by this build.
    #[serde(other)]
    Other,
}

/// Why a ticket was voided.
///
/// What: the first three arrive on `secrets.ticket.void` from a hook; the
/// rest the server applies itself and refuses on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VoidCause {
    /// A main-thread PreToolUse in the session.
    PreToolUse,
    /// A subagent's or teammate's PreToolUse in the session.
    SubagentPreToolUse,
    /// The session's Stop.
    Stop,
    /// [`TICKET_TTL`] passed.
    Ttl,
    /// A second mint in the session or for the same `claude`.
    SecondMint,
    /// A redemption attempt that did not match the ticket.
    Mismatch,
    /// A cause a newer server records. Never written by this build.
    #[serde(other)]
    Other,
}

impl VoidCause {
    /// Whether a hook may send this cause on `secrets.ticket.void`.
    pub(crate) fn hook_may_send(self) -> bool {
        matches!(
            self,
            Self::PreToolUse | Self::SubagentPreToolUse | Self::Stop
        )
    }
}

/// Which mint check refused, on a `refuse` record (DOC-86 §2, §6).
///
/// Test: `mint_from_a_non_tm_exe_is_refused`, `mint_from_a_grandchild_is_refused`,
/// `mint_from_an_old_process_is_refused`, `mint_refused_while_mint_disabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TicketRefusal {
    /// `mint_enabled` is `false`.
    MintDisabled,
    /// The kernel reported no peer pid.
    NoPeer,
    /// The peer is not the installed `tm` in the cargo bin directory.
    NotInstalledTm,
    /// The peer's argv is not the registered hook argv.
    NotHookArgv,
    /// The peer's parent is not a Claude Code process.
    ParentNotClaude,
    /// The peer started more than two seconds before the mint.
    PeerTooOld,
    /// The parent `claude` has no valid launch record; the record's
    /// `architect_refusal` names the failed check.
    LaunchRecord,
    /// The project did not resolve.
    Project,
    /// The process table could not be read.
    ProcessLookup,
    /// The clock failed, or a start time lies in the future.
    Clock,
    /// A ticket was already pending; both are void.
    Pending,
    /// [`MAX_LIVE_TICKETS`] live tickets exist.
    Capacity,
    /// The store lock is poisoned, or the random source failed.
    Unavailable,
    /// An arm a newer server records. Never written by this build.
    #[serde(other)]
    Other,
}

/// The `ticket` object of an audit record (DOC-86 §6).
///
/// What: ids are server-validated identifiers ([`super::ticket_checks`]),
/// the argv as its SHA-256 only. The project and key go in the record's own
/// `project_root` and `key`. `None` fields are not written. No value, no
/// environment and no argument other than the key name.
/// Test: `ticket_records_carry_no_value_and_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TicketAudit {
    /// What happened.
    pub event: TicketEvent,
    /// The ticket's kind, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<TicketKind>,
    /// The server's id for the ticket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
    /// Claude Code's `session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Claude Code's `prompt_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    /// Claude Code's `tool_use_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// The `claude` the ticket is bound to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_pid: Option<u32>,
    /// The method the ticket is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<TicketOp>,
    /// SHA-256 of the redeemer's argv, lowercase hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv_sha256: Option<String>,
    /// Which mint check refused, on a `refuse` record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_arm: Option<TicketRefusal>,
    /// Why the ticket was voided, on a `void` record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub void_cause: Option<VoidCause>,
}

impl TicketAudit {
    /// An `event` record with no subject.
    pub fn new(event: TicketEvent) -> Self {
        Self {
            event,
            kind: None,
            ticket_id: None,
            session_id: None,
            prompt_id: None,
            tool_use_id: None,
            claude_pid: None,
            op: None,
            argv_sha256: None,
            reason_arm: None,
            void_cause: None,
        }
    }
}

/// One live ticket (DOC-86 §2). It is live from mint until removed.
#[derive(Debug, Clone)]
pub(crate) struct Ticket {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) prompt_id: String,
    pub(crate) tool_use_id: String,
    pub(crate) claude_pid: u32,
    pub(crate) claude_start: StartTime,
    pub(crate) kind: TicketKind,
    pub(crate) argv_sha256: String,
    pub(crate) op: TicketOp,
    pub(crate) project: PathBuf,
    pub(crate) key: SecretKey,
    pub(crate) minted_at: Duration,
}

impl Ticket {
    /// This ticket's audit object for `event`.
    pub(crate) fn audit(&self, event: TicketEvent) -> TicketAudit {
        TicketAudit {
            kind: Some(self.kind),
            ticket_id: Some(self.id.clone()),
            session_id: Some(self.session_id.clone()),
            prompt_id: Some(self.prompt_id.clone()),
            tool_use_id: Some(self.tool_use_id.clone()),
            claude_pid: Some(self.claude_pid),
            op: Some(self.op),
            argv_sha256: Some(self.argv_sha256.clone()),
            ..TicketAudit::new(event)
        }
    }

    /// Whether [`TICKET_TTL`] has passed at `now`.
    fn expired(&self, now: Duration) -> bool {
        now.saturating_sub(self.minted_at) >= TICKET_TTL || now < self.minted_at
    }
}

/// The live tickets, as [`TicketStore::locked`] hands them out.
#[derive(Debug, Default)]
pub(crate) struct Tickets(Vec<Ticket>);

impl Tickets {
    /// Remove and return every ticket expired at `now`.
    ///
    /// What: a clock that moved back past a mint also expires it (fail
    /// closed).
    pub(crate) fn take_expired(&mut self, now: Duration) -> Vec<Ticket> {
        self.take_where(|t| t.expired(now))
    }

    /// Remove and return every ticket of `session`.
    pub(crate) fn take_session(&mut self, session: &str) -> Vec<Ticket> {
        self.take_where(|t| t.session_id == session)
    }

    /// Remove and return every ticket of `session` or of `claude` (pid).
    pub(crate) fn take_session_or_claude(&mut self, session: &str, claude: u32) -> Vec<Ticket> {
        self.take_where(|t| t.session_id == session || t.claude_pid == claude)
    }

    /// Remove and return the ticket bound to (`claude`, `start`), if any.
    pub(crate) fn take_claude(&mut self, claude: u32, start: StartTime) -> Option<Ticket> {
        let index = self
            .0
            .iter()
            .position(|t| t.claude_pid == claude && t.claude_start == start)?;
        Some(self.0.swap_remove(index))
    }

    /// Whether a ticket is pending, for `session` or for any session.
    pub(crate) fn pending(&self, session: Option<&str>) -> bool {
        self.0
            .iter()
            .any(|t| session.is_none_or(|s| t.session_id == s))
    }

    /// How many tickets are live.
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// Store `ticket`.
    pub(crate) fn insert(&mut self, ticket: Ticket) {
        self.0.push(ticket);
    }

    fn take_where(&mut self, take: impl Fn(&Ticket) -> bool) -> Vec<Ticket> {
        let (taken, kept) = std::mem::take(&mut self.0).into_iter().partition(take);
        self.0 = kept;
        taken
    }
}

/// Why the store could not be used. Each one refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreError {
    /// The lock is poisoned.
    Poisoned,
    /// The clock failed.
    Clock,
}

/// The daemon's in-memory ticket store (DOC-86 §2).
///
/// Why: see the module docs.
/// What: one mutex over [`Tickets`] and the clock the TTL counts on. A
/// poisoned lock is never recovered.
/// Test: `ticket_tests.rs`.
pub struct TicketStore {
    tickets: Mutex<Tickets>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for TicketStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicketStore").finish_non_exhaustive()
    }
}

impl TicketStore {
    /// An empty store whose TTL counts on `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            tickets: Mutex::new(Tickets::default()),
            clock,
        }
    }

    /// The production store: empty, on the system clock.
    pub fn os_default() -> Self {
        Self::new(Arc::new(SystemClock))
    }

    /// Now, on the store's clock.
    pub(crate) fn now(&self) -> Result<Duration, StoreError> {
        self.clock.now().map_err(|_| StoreError::Clock)
    }

    /// Run `body` under the store lock.
    ///
    /// Why: a check, its audit record and the change it allows happen under
    /// one lock, so two racing calls cannot both pass a one-use check.
    /// What: [`StoreError::Poisoned`] for a poisoned lock; `body` never runs
    /// then. The clock is read by `body` ([`Self::now`]), so a void still
    /// removes tickets when the clock fails.
    pub(crate) fn locked<R>(&self, body: impl FnOnce(&mut Tickets) -> R) -> Result<R, StoreError> {
        let mut guard: MutexGuard<'_, Tickets> =
            self.tickets.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(body(&mut guard))
    }
}
