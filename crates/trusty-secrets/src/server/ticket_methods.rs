//! The ticket method bodies — `secrets.ticket.mint`, `secrets.ticket.void`,
//! `secrets.ticket.pending` — and the redemption `set` and a local `copy`
//! run (#10459 PR1, DOC-86 §2, §6).
//!
//! Why: DOC-86 §2. The Architect main thread's PreToolUse hook mints a
//! ticket for one `tm secrets set|copy` call; every PreToolUse and Stop in
//! the session voids a pending one; the pinned pane senders ask whether one
//! is pending (DOC-86 §4). A `set` or local `copy` from under the launched
//! Architect writes a denied key only after it redeems one.
//! What: every mint check fails closed with [`ErrorKind::TicketRefused`]
//! (the record's `reason_arm` names the check) and leaves a best-effort
//! `refuse` record. A mint writes its record before the ticket is stored,
//! and a redemption before the write it allows; a record that cannot be
//! written is [`ErrorKind::AuditUnavailable`] and nothing is stored or
//! written. A void removes the tickets first, since "no ticket" is the
//! closed state, then answers `AuditUnavailable` if a record failed. A void
//! that found nothing writes no record. `pending` writes none; its only
//! records are the TTL voids it prunes.
//! Test: `ticket_tests.rs` beside `wire_tests.rs`.

use std::path::PathBuf;

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::architect::{ArchitectRefusal, check_architect};
use super::audit::{AuditMethod, AuditRecord};
use super::errors::ErrorKind;
use super::gate::{suppressed_by_machine, unaudited_allow};
use super::grant::hex;
use super::methods::{Caller, decode, split_project, to_json};
use super::project::ProjectContext;
use super::router::State;
use super::ticket::{
    MAX_LIVE_TICKETS, TICKET_TTL, Ticket, TicketAudit, TicketEvent, TicketKind, TicketOp,
    TicketRefusal, VoidCause,
};
use super::ticket_checks::{TicketCall, check_mint_peer, check_redeemer, valid_id, valid_sha256};
use crate::api::SecretKey;
use crate::store::platform::now_unix;

/// `secrets.ticket.mint` params beside `project`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MintRequest {
    session_id: String,
    prompt_id: String,
    tool_use_id: String,
    kind: TicketKind,
    /// [`super::ticket_checks::argv_sha256`] of the call's argv.
    argv_sha256: String,
    op: TicketOp,
    key: SecretKey,
}

#[derive(Debug, Serialize)]
struct MintResponse {
    ticket_id: String,
    /// Unix seconds.
    expires_at: u64,
}

/// `secrets.ticket.void` params.
// #10459 PR1 fix round: no `deny_unknown_fields` here, unlike the mint, so
// an extra field from a newer hook never leaves a ticket live.
#[derive(Debug, Deserialize)]
struct VoidRequest {
    session_id: String,
    cause: VoidCause,
}

/// `secrets.ticket.pending` params; no session asks about every session.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRequest {
    #[serde(default)]
    session_id: Option<String>,
}

/// One ticket record's subject.
#[derive(Debug, Clone)]
struct Entry {
    audit: TicketAudit,
    project: Option<PathBuf>,
    key: Option<SecretKey>,
    architect_refusal: Option<ArchitectRefusal>,
}

impl Entry {
    fn of(ticket: &Ticket, event: TicketEvent) -> Self {
        Self {
            audit: ticket.audit(event),
            project: Some(ticket.project.clone()),
            key: Some(ticket.key.clone()),
            architect_refusal: None,
        }
    }
}

/// Writes ticket records to the #4567 trail, one JSON line each.
struct TicketLog<'a> {
    state: &'a State,
    caller_pid: Option<u32>,
    suppressed: bool,
}

impl<'a> TicketLog<'a> {
    fn new(state: &'a State, caller: Caller) -> Self {
        Self {
            state,
            caller_pid: caller.pid(),
            suppressed: suppressed_by_machine(state),
        }
    }

    /// Append one record; `Err` is [`ErrorKind::AuditUnavailable`] under
    /// the gate's fail-closed rule. A machine config that turns the audit
    /// off writes nothing.
    fn write(
        &self,
        method: AuditMethod,
        entry: Entry,
        denied: Option<ErrorKind>,
    ) -> Result<(), ErrorKind> {
        if self.suppressed {
            return Ok(());
        }
        let mut record =
            AuditRecord::new(now_unix(), method, denied).with_caller_pid(self.caller_pid);
        record.project_root = entry.project;
        record.key = entry.key;
        record.architect_refusal = entry.architect_refusal;
        record.ticket = Some(entry.audit);
        let appended = self
            .state
            .audit
            .open()
            .ok()
            .and_then(|mut file| file.append(&record).ok());
        match appended {
            Some(()) => Ok(()),
            None => unaudited_allow(),
        }
    }

    /// Record a refused mint, best-effort, and answer `kind`.
    fn refuse(&self, mut entry: Entry, arm: Option<TicketRefusal>, kind: ErrorKind) -> ErrorKind {
        entry.audit.event = TicketEvent::Refuse;
        entry.audit.reason_arm = arm;
        let _ = self.write(AuditMethod::TicketMint, entry, Some(kind));
        kind
    }

    /// Record `ticket` voided with `cause`.
    fn void(&self, ticket: &Ticket, cause: VoidCause) -> Result<(), ErrorKind> {
        let mut entry = Entry::of(ticket, TicketEvent::Void);
        entry.audit.void_cause = Some(cause);
        self.write(AuditMethod::TicketVoid, entry, None)
    }

    /// Record every ticket in `tickets` voided with `cause`, best-effort;
    /// they are already removed.
    fn void_all(&self, tickets: &[Ticket], cause: VoidCause) {
        for ticket in tickets {
            let _ = self.void(ticket, cause);
        }
    }
}

/// Decode a whole params object as `T`.
fn decode_object<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ErrorKind> {
    match params {
        Value::Object(fields) => decode(fields),
        _ => Err(ErrorKind::InvalidParams),
    }
}

/// A random ticket id: 128 bits, hex. Not a bearer secret.
fn new_ticket_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    OsRng.try_fill_bytes(&mut bytes).ok()?;
    Some(hex(&bytes))
}

/// Why a mint stopped inside the store lock.
enum Stop {
    Refused(TicketRefusal),
    Audit(ErrorKind),
}

/// `secrets.ticket.mint`: store a ticket for one main-thread call.
///
/// Why: DOC-86 §2 — the only way a denied key opens to the Architect.
/// What: params are `project` beside [`MintRequest`]; an id that
/// [`valid_id`] refuses, or an argv hash that is not 64 lowercase hex, is
/// [`ErrorKind::InvalidParams`]. Then, each [`ErrorKind::TicketRefused`]
/// with its arm: `mint_enabled` false; no peer pid; the project unresolved;
/// a clock error; [`check_mint_peer`]; the parent `claude`'s launch record
/// ([`check_architect`], its refusal on the record); its start time. Under
/// the store lock: expired tickets are voided (`ttl`); a ticket pending for
/// the session or that `claude` is voided (`second_mint`) and the mint
/// refused (`pending`); [`MAX_LIVE_TICKETS`] refuses. The `mint` record is
/// written, then the ticket stored. The reply is its id and expiry.
/// Test: `mint_refused_while_mint_disabled`, `mint_from_a_non_tm_exe_is_refused`,
/// `mint_from_a_grandchild_is_refused`, `mint_from_an_old_process_is_refused`,
/// `second_mint_voids_both`, `audit_unwritable_at_mint_refuses`,
/// `process_table_error_at_mint_refuses`, `clock_error_refuses`,
/// `mint_with_an_agent_id_is_invalid`.
pub(crate) fn mint(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    let log = TicketLog::new(state, caller);
    let mut entry = Entry {
        audit: TicketAudit::new(TicketEvent::Refuse),
        project: None,
        key: None,
        architect_refusal: None,
    };
    let decoded = split_project(params)
        .and_then(|(dir, rest)| Ok((dir, decode::<MintRequest>(rest)?)))
        .and_then(|(dir, request)| {
            let valid = [
                &request.session_id,
                &request.prompt_id,
                &request.tool_use_id,
            ]
            .into_iter()
            .all(|id| valid_id(id))
                && valid_sha256(&request.argv_sha256);
            valid
                .then_some((dir, request))
                .ok_or(ErrorKind::InvalidParams)
        });
    let (dir, request) = decoded.map_err(|kind| log.refuse(entry.clone(), None, kind))?;
    entry.audit = TicketAudit {
        kind: Some(request.kind),
        session_id: Some(request.session_id.clone()),
        prompt_id: Some(request.prompt_id.clone()),
        tool_use_id: Some(request.tool_use_id.clone()),
        op: Some(request.op),
        argv_sha256: Some(request.argv_sha256.clone()),
        ..TicketAudit::new(TicketEvent::Refuse)
    };
    entry.key = Some(request.key.clone());
    let refused =
        |entry: &Entry, arm| log.refuse(entry.clone(), Some(arm), ErrorKind::TicketRefused);
    // #10459 PR1: off until PR 5; checked after the decode, so a bad
    // request still reads as one.
    if !state.settings.mint_enabled {
        return Err(refused(&entry, TicketRefusal::MintDisabled));
    }
    let peer = caller
        .pid()
        .ok_or_else(|| refused(&entry, TicketRefusal::NoPeer))?;
    let project = ProjectContext::resolve(state, &dir)
        .map_err(|_| refused(&entry, TicketRefusal::Project))?;
    entry.project = Some(project.root().to_path_buf());
    let table = state.grants.processes();
    let now = state
        .tickets
        .now()
        .map_err(|_| refused(&entry, TicketRefusal::Clock))?;
    let claude = check_mint_peer(table, state.tm_bin_dir.as_deref(), peer, now)
        .map_err(|arm| refused(&entry, arm))?;
    entry.audit.claude_pid = Some(claude);
    match check_architect(table, state.architect_records.as_deref(), peer) {
        Ok(recorded) if recorded == claude => {}
        Ok(_) => return Err(refused(&entry, TicketRefusal::ParentNotClaude)),
        Err(refusal) => {
            entry.architect_refusal = Some(refusal);
            return Err(refused(&entry, TicketRefusal::LaunchRecord));
        }
    }
    let claude_start = table
        .start_time(claude)
        .map_err(|_| refused(&entry, TicketRefusal::ProcessLookup))?;
    let id = new_ticket_id().ok_or_else(|| refused(&entry, TicketRefusal::Unavailable))?;
    entry.audit.ticket_id = Some(id.clone());
    let stored = state.tickets.locked(|tickets| {
        let now = state
            .tickets
            .now()
            .map_err(|_| Stop::Refused(TicketRefusal::Clock))?;
        log.void_all(&tickets.take_expired(now), VoidCause::Ttl);
        // #10459: DOC-86 §8 — a second mint voids both.
        let pending = tickets.take_session_or_claude(&request.session_id, claude);
        if !pending.is_empty() {
            log.void_all(&pending, VoidCause::SecondMint);
            return Err(Stop::Refused(TicketRefusal::Pending));
        }
        if tickets.len() >= MAX_LIVE_TICKETS {
            return Err(Stop::Refused(TicketRefusal::Capacity));
        }
        let ticket = Ticket {
            id,
            session_id: request.session_id,
            prompt_id: request.prompt_id,
            tool_use_id: request.tool_use_id,
            claude_pid: claude,
            claude_start,
            kind: request.kind,
            argv_sha256: request.argv_sha256,
            op: request.op,
            project: project.root().to_path_buf(),
            key: request.key,
            minted_at: now,
        };
        // #10459: write-ahead — no record, no ticket.
        log.write(
            AuditMethod::TicketMint,
            Entry::of(&ticket, TicketEvent::Mint),
            None,
        )
        .map_err(Stop::Audit)?;
        let response = MintResponse {
            ticket_id: ticket.id.clone(),
            expires_at: now.saturating_add(TICKET_TTL).as_secs(),
        };
        tickets.insert(ticket);
        Ok(response)
    });
    match stored {
        Ok(Ok(response)) => to_json(&response),
        Ok(Err(Stop::Refused(arm))) => Err(refused(&entry, arm)),
        Ok(Err(Stop::Audit(kind))) => Err(kind),
        Err(_poisoned) => Err(refused(&entry, TicketRefusal::Unavailable)),
    }
}

/// `secrets.ticket.void`: remove every ticket of a session.
///
/// Why: DOC-86 §2 — every PreToolUse in the session (subagents and
/// teammates included) and its Stop void a pending ticket, so it can be
/// redeemed only by the call it was minted for.
/// What: params are [`VoidRequest`], extra fields ignored; a malformed
/// session id, or a cause a hook may not send, is
/// [`ErrorKind::InvalidParams`]. Expired tickets are
/// voided (`ttl`) when the clock reads; the session's tickets are removed
/// whatever the clock says, each with a `void` record. The reply is the
/// count; a record that failed makes it [`ErrorKind::AuditUnavailable`],
/// the tickets already gone. Any caller may void.
/// Test: `next_event_voids_a_pending_ticket`,
/// `subagent_pretooluse_voids_pending_ticket`, `decline_then_stop_voids`,
/// `audit_unwritable_at_void_refuses`, `void_with_an_extra_field_still_voids`.
pub(crate) fn void(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    let request: VoidRequest = decode_object(params)?;
    if !valid_id(&request.session_id) || !request.cause.hook_may_send() {
        return Err(ErrorKind::InvalidParams);
    }
    let log = TicketLog::new(state, caller);
    let (voided, recorded) = state
        .tickets
        .locked(|tickets| {
            if let Ok(now) = state.tickets.now() {
                log.void_all(&tickets.take_expired(now), VoidCause::Ttl);
            }
            let taken = tickets.take_session(&request.session_id);
            let recorded = taken
                .iter()
                .map(|ticket| log.void(ticket, request.cause))
                .fold(Ok(()), Result::and);
            (taken.len(), recorded)
        })
        .map_err(|_| ErrorKind::Internal)?;
    recorded?;
    Ok(serde_json::json!({ "voided": voided }))
}

/// `secrets.ticket.pending`: whether a ticket is pending.
///
/// Why: DOC-86 §4 — a pinned pane sender skips its send while a ticket is
/// pending, and on any error.
/// What: params are [`PendingRequest`]. Expired tickets are voided first.
/// A poisoned lock or a clock error is [`ErrorKind::Internal`], never
/// `false`.
/// Test: `ttl_expiry_voids`, `restart_drops_tickets`.
pub(crate) fn pending(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    let request: PendingRequest = decode_object(params)?;
    if request
        .session_id
        .as_deref()
        .is_some_and(|id| !valid_id(id))
    {
        return Err(ErrorKind::InvalidParams);
    }
    let log = TicketLog::new(state, caller);
    let pending = state
        .tickets
        .locked(|tickets| {
            let now = state.tickets.now().map_err(|_| ErrorKind::Internal)?;
            log.void_all(&tickets.take_expired(now), VoidCause::Ttl);
            Ok::<_, ErrorKind>(tickets.pending(request.session_id.as_deref()))
        })
        .map_err(|_| ErrorKind::Internal)??;
    Ok(serde_json::json!({ "pending": pending }))
}

/// Redeem the ticket of `claude` for `call` by `peer`.
///
/// Why: DOC-86 §2, Q4 — the launch record alone never opens a denied key.
/// What: `Ok(None)` when redeemed: under the store lock, expired tickets are
/// voided, the ticket bound to (`claude`, its start time) is removed —
/// one use, matched or not — [`check_redeemer`] passes, and the `redeem`
/// record is written before the caller's write. `Ok(Some(refusal))` keeps
/// the agent rules: no ticket ([`ArchitectRefusal::NoTicket`]), a mismatch
/// (the ticket is voided `mismatch`), an unreadable process, or a store or
/// clock error ([`ArchitectRefusal::TicketUnavailable`]). `Err` is
/// [`ErrorKind::AuditUnavailable`]: the ticket is spent, nothing written.
/// Test: `architect_with_a_valid_record_sets_a_denied_key`,
/// `record_without_ticket_keeps_agent_rules`, `ticket_replay_is_refused`,
/// `audit_unwritable_at_redeem_refuses`.
pub(crate) fn redeem(
    state: &State,
    caller: Caller,
    peer: u32,
    claude: u32,
    call: &TicketCall<'_>,
) -> Result<Option<ArchitectRefusal>, ErrorKind> {
    let log = TicketLog::new(state, caller);
    let table = state.grants.processes();
    let Ok(claude_start) = table.start_time(claude) else {
        return Ok(Some(ArchitectRefusal::ProcessLookup));
    };
    let outcome = state.tickets.locked(|tickets| {
        let Ok(now) = state.tickets.now() else {
            return Ok(Some(ArchitectRefusal::TicketUnavailable));
        };
        log.void_all(&tickets.take_expired(now), VoidCause::Ttl);
        let Some(ticket) = tickets.take_claude(claude, claude_start) else {
            return Ok(Some(ArchitectRefusal::NoTicket));
        };
        match check_redeemer(table, &ticket, peer, call) {
            Err(refusal) => {
                let mut entry = Entry::of(&ticket, TicketEvent::Void);
                entry.audit.void_cause = Some(VoidCause::Mismatch);
                entry.architect_refusal = Some(refusal);
                let _ = log.write(AuditMethod::TicketVoid, entry, None);
                Ok(Some(refusal))
            }
            // #10459: write-ahead — the record precedes the write it allows.
            Ok(()) => log
                .write(
                    AuditMethod::TicketRedeem,
                    Entry::of(&ticket, TicketEvent::Redeem),
                    None,
                )
                .map(|()| None),
        }
    });
    outcome.unwrap_or(Ok(Some(ArchitectRefusal::TicketUnavailable)))
}
