//! The flag-set method body: `secrets.set_agents_may_use` (DOC-74 §15.8,
//! S8 slice 3, #9070).
//!
//! Why: the "agents may use" flag decides which keys a grant registered
//! under Claude Code may name. Turning it on widens what an agent can read,
//! so the server judges the caller and records the change before making it.
//! What: one body, [`set_agents_may_use`]. A caller whose own ancestry has a
//! Claude Code process cannot turn a flag on; turning one off is allowed,
//! because it narrows access. The allow record is written before the backend
//! call (Architect ruling 2026-10-10), and a failed backend call adds a deny
//! record with its kind. #9066: a call through the console bridge is judged
//! on the console's verdict about its HTTP client instead of the peer's own
//! ancestry (`bridge`), so the console route gets the same rule.
//! Test: `flag_tests.rs` beside `server_tests.rs`.

use serde_json::Value;

use super::ancestry::has_agent_ancestor;
use super::audit::AuditMethod;
use super::errors::ErrorKind;
use super::gate::{Recording, audited};
use super::methods::{Caller, decode, split_project, to_json};
use super::project::ProjectContext;
use super::router::State;
use crate::api::methods::{SetAgentsMayUseRequest, SetAgentsMayUseResponse};
use crate::store::SecretStore;

/// `secrets.set_agents_may_use`: turn one key's flag on or off.
///
/// Why: DOC-74 §15.8 — an agent must not flag a key for itself, and every
/// flag change must leave a record even when the change then fails.
/// What: params are `project` beside [`SetAgentsMayUseRequest`]. The caller
/// is the socket peer; with none, [`ErrorKind::GrantRefused`]. The vault must
/// be in the project's scope. To turn the flag on, [`has_agent_ancestor`]
/// is read on the caller's own ancestry from the registry's process table: a
/// read error is [`ErrorKind::GrantRefused`], an agent ancestor is
/// [`ErrorKind::AgentUseRefused`]. #9066: a bridge call takes that answer
/// from the console's verdict (`BridgeVerdict::judged`) instead. Each refusal is one deny record. Then
/// under the key's index lock (#9621), after its row check,
/// [`super::gate::Gate::write_ahead`] appends the allow record; if the log
/// cannot be opened or the record appended, the reply is
/// [`ErrorKind::AuditUnavailable`] and the backend is never called. An
/// unindexed key is [`ErrorKind::NotFound`] with one deny record and no
/// allow. A backend failure after the allow record (a backend with no flag
/// item refuses on with [`ErrorKind::Unsupported`]) appends a second, deny
/// record.
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
            if request.allowed {
                let agent_parent = match caller.bridge() {
                    // #9066: the console judged the client it serves.
                    Some(verdict) => verdict.judged()?,
                    // #9070: an unreadable ancestor refuses; never skipped.
                    None => has_agent_ancestor(state.grants.processes(), peer)
                        .map_err(|_| ErrorKind::GrantRefused)?,
                };
                gate.agent_parent(agent_parent);
                if agent_parent {
                    return Err(ErrorKind::AgentUseRefused);
                }
            }
            let store = SecretStore::new(project.backend(state)?, state.index.clone());
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
