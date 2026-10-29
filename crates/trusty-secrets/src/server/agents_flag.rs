//! The flag-set method body: `secrets.set_agents_may_use` (DOC-74 §15.8,
//! S8 slice 3, #9070).
//!
//! Why: #10386 — a key is usable by agents unless the operator denied it.
//! The wire keeps the pre-#10386 shape: `allowed: false` writes the key's
//! deny, `allowed: true` removes it. Removing a deny widens what an agent can
//! read, so the server judges the caller and records the change before
//! making it.
//! What: [`set_agents_may_use`] and [`refuse_agent_caller`], the agent
//! refusal every operator-only method shares. A caller whose own ancestry has
//! a Claude Code process cannot remove a deny; setting one is allowed,
//! because it narrows access. The allow record is written before the backend
//! call (Architect ruling 2026-10-10), and a failed backend call adds a deny
//! record with its kind. #9066: a call through the console bridge is judged
//! on the console's verdict about its HTTP client instead of the peer's own
//! ancestry (`bridge`), so the console route gets the same rule.
//! Test: `flag_tests.rs` beside `server_tests.rs`.

use serde_json::Value;

use super::ancestry::has_agent_ancestor;
use super::architect::check_architect;
use super::audit::AuditMethod;
use super::errors::ErrorKind;
use super::gate::{Gate, Recording, audited};
use super::methods::{Caller, decode, split_project, to_json};
use super::project::ProjectContext;
use super::router::State;
use crate::api::methods::{SetAgentsMayUseRequest, SetAgentsMayUseResponse};
use crate::store::SecretStore;

/// `secrets.set_agents_may_use`: remove (`allowed`) or set one key's deny.
///
/// Why: DOC-74 §15.8 — an agent must not lift an operator's deny, and every
/// deny change must leave a record even when the change then fails.
/// What: params are `project` beside [`SetAgentsMayUseRequest`]. The caller
/// is the socket peer; with none, [`ErrorKind::GrantRefused`]. The vault must
/// be in the project's scope. To remove a deny, [`refuse_agent_caller`]
/// judges the caller. Each refusal is one deny record. Then
/// under the key's index lock (#9621), after its row check,
/// [`super::gate::Gate::write_ahead`] appends the allow record; if the log
/// cannot be opened or the record appended, the reply is
/// [`ErrorKind::AuditUnavailable`] and the backend is never called. An
/// unindexed key is [`ErrorKind::NotFound`] with one deny record and no
/// allow. A backend failure after the allow record (a backend with no
/// control item refuses a deny with [`ErrorKind::Unsupported`]) appends a
/// second, deny record.
/// Test: `set_flag_on_by_agent_ancestor_is_refused_and_audited`,
/// `set_flag_off_by_agent_ancestor_is_allowed`,
/// `set_flag_allow_record_is_written_before_the_backend_call`,
/// `set_flag_with_unwritable_audit_changes_nothing`,
/// `set_flag_append_failure_after_open_changes_nothing`,
/// `set_flag_backend_failure_leaves_a_deny_after_the_allow_record`,
/// `set_flag_on_unindexed_key_leaves_one_deny_and_no_allow`,
/// `set_flag_unreadable_ancestry_refuses_on`,
/// `bridge_verdict_parity_with_direct_socket_table`.
pub(crate) fn set_agents_may_use(
    state: &State,
    caller: Caller,
    params: Value,
) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::SetAgentsMayUse,
        Recording::WriteAhead,
        |gate| {
            let (dir, rest) = split_project(params)?;
            let request: SetAgentsMayUseRequest = decode(rest)?;
            gate.name(&request.vault, Some(&request.key));
            gate.agents_allowed(request.allowed);
            // #9070: the caller is the kernel's peer, never a param.
            let peer = caller.pid().ok_or(ErrorKind::GrantRefused)?;
            let project = ProjectContext::resolve(state, &dir)?;
            gate.project(&project);
            project.require_in_scope(&request.vault)?;
            // #10386: removing a deny widens access, so only a person may.
            if request.allowed {
                refuse_agent_caller(state, caller, peer, gate)?;
            }
            let store = SecretStore::new(project.backend(state)?, state.index.clone())
                .with_deny_audit(gate.deny_audit());
            // #9070: no record, no change (write-ahead, Architect Q1).
            // #9621: written under the key's index lock, so records and
            // changes for one key land in the same order.
            store.set_agents_may_use_after(
                &request.vault,
                &request.key,
                request.allowed,
                || gate.write_ahead(),
            )??;
            to_json(&SetAgentsMayUseResponse {
                vault: request.vault,
                key: request.key,
                allowed: request.allowed,
            })
        },
    )
}

/// Refuse a caller whose ancestry has a Claude Code process (#10386).
///
/// Why: Architect ruling 2026-10-28 (Q2, Q3) — an operator-only change
/// (removing a deny, `delete`, a copy to a sync target) stays a person's.
/// #10459: `set` and a local `copy` no longer call it; they judge the caller
/// with [`judge_agent_caller`] and check the key's deny instead.
/// What: [`judge_agent_caller`], then an agent ancestor is
/// [`ErrorKind::AgentUseRefused`].
/// Test: `set_flag_on_by_agent_ancestor_is_refused_and_audited`,
/// `set_flag_unreadable_ancestry_refuses_on`,
/// `delete_by_agent_ancestor_is_refused_before_the_backend`,
/// `server_copy_to_sync_target_by_agent_descendant_is_refused_and_audited`.
pub(crate) fn refuse_agent_caller(
    state: &State,
    caller: Caller,
    peer: u32,
    gate: &mut Gate<'_>,
) -> Result<(), ErrorKind> {
    if judge_agent_caller(state, caller, peer, gate)? {
        return Err(ErrorKind::AgentUseRefused);
    }
    Ok(())
}

/// Whether the caller has a Claude Code ancestor (#10386, #10459).
///
/// Why: a refusal the server cannot judge is a refusal (Fail-Open Check),
/// for both the operator-only methods and an agent's write of a key.
/// What: `peer` is the socket peer's pid. On a console bridge call the
/// console's verdict is judged (`BridgeVerdict::judged`), else
/// [`has_agent_ancestor`] on the peer's ancestry; an unreadable chain is
/// [`ErrorKind::GrantRefused`]. The verdict goes on the gate's records.
/// Test: `set_and_delete_with_unreadable_ancestry_or_no_peer_are_refused`,
/// `set_by_agent_ancestor_of_an_undenied_key_succeeds`.
pub(crate) fn judge_agent_caller(
    state: &State,
    caller: Caller,
    peer: u32,
    gate: &mut Gate<'_>,
) -> Result<bool, ErrorKind> {
    let agent_parent = match caller.bridge() {
        // #9066: the console judged the client it serves.
        Some(verdict) => verdict.judged()?,
        // #9070: an unreadable ancestor refuses; never skipped.
        None => has_agent_ancestor(state.grants.processes(), peer)
            .map_err(|_| ErrorKind::GrantRefused)?,
    };
    gate.agent_parent(agent_parent);
    Ok(agent_parent)
}

/// Who a `set` or a local `copy` writes as (#10459).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Writer {
    /// No Claude Code ancestor: the operator's rules.
    Person,
    /// Under Claude Code: keys with no deny only.
    Agent,
    /// Under the `claude` tm launched as the Architect: any key.
    Architect,
}

/// The [`Writer`] for a caller [`judge_agent_caller`] already judged.
///
/// Why: #10459 P1 (Architect clearance 2026-10-29 22:58Z) — the Architect
/// may write keys denied to agents; every other agent may not.
/// What: `agent == false` is [`Writer::Person`]. An agent through the
/// console bridge is [`Writer::Agent`]: its peer is the console, not the
/// client (a console under Claude Code is already `BridgeRefused`; this arm
/// keeps the rule if that ever changes). Otherwise [`check_architect`] on the peer: `Ok` marks the gate's
/// records `architect: true` and is [`Writer::Architect`]; any refusal is
/// [`Writer::Agent`] (fail closed). Only `set` and a local `copy` call this;
/// `delete`, deny removal and a sync-target push never do.
/// Test: `architect_with_a_valid_record_sets_a_denied_key`,
/// `architect_refusals_each_keep_the_agent_rules`,
/// `nested_claude_under_the_architect_is_not_the_architect`,
/// `bridge_call_from_under_the_architect_is_refused`.
pub(crate) fn writer_for(
    state: &State,
    caller: Caller,
    peer: u32,
    gate: &mut Gate<'_>,
    agent: bool,
) -> Writer {
    if !agent {
        return Writer::Person;
    }
    // #10459: the bridge's peer is the console; its client is never judged here.
    if caller.bridge().is_some() {
        return Writer::Agent;
    }
    let dir = state.architect_records.as_deref();
    match check_architect(state.grants.processes(), dir, peer) {
        Ok(()) => {
            gate.architect();
            Writer::Architect
        }
        Err(_) => Writer::Agent,
    }
}
