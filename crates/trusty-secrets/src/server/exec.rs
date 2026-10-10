//! The exec-grant method bodies: `secrets.grant`, `secrets.resolve` and
//! `secrets.revoke` (DOC-74 §15.8, S8 slice 2, #9070).
//!
//! Why: tier 3 of DOC-74 §15.8. `tm secrets exec` spawns a child, registers
//! a grant for it here (ruling 34: the grant lives on this socket and tm is
//! a client; the server mints the token), passes the token to the child
//! through its environment, and execs. The child, or any descendant, then
//! reads one value per call with `secrets.resolve`, the only method that
//! returns a value. tm removes the grant with `secrets.revoke` when the
//! child exits.
//! What: every body takes the caller's pid from [`Caller`], the socket peer
//! the router read, never from the request. Every refusal of a grant check
//! (unknown, expired or revoked token, key outside the grant, caller outside
//! the child's tree, no peer pid) is the one [`ErrorKind::GrantRefused`] and
//! carries no value. Each call leaves audit records on the #4567 trail:
//! `grant`, one per granted key or one deny; `resolve`, one per call (one
//! key), allow or deny, plus a deny after the allow when the grant is gone
//! by the time it is spent (#9621); `revoke`, one per call. An allowed `grant` or
//! `resolve` whose record cannot be written returns no token and no value
//! (`gate`'s fail-closed rule).
//! Test: `wire_tests.rs` beside `server_tests.rs`.

use std::slice;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::ancestry::has_agent_ancestor;
use super::audit::AuditMethod;
use super::errors::ErrorKind;
use super::gate::{Recording, audited};
use super::grant::{GrantError, GrantRequest, GrantScope, GrantToken};
use super::methods::{Caller, decode, split_project, to_json};
use super::project::ProjectContext;
use super::router::State;
use crate::api::SecretRef;
use crate::api::methods::{
    ExecGrantRequest, ExecGrantResponse, ResolveRequest, ResolveResponse, RevokeRequest,
    RevokeResponse,
};
use crate::store::SecretStore;
use crate::store::resolve::{require_agent_use, resolve_reference};

/// Fold a registry error into its wire kind.
///
/// What: a refusal and an unplaceable process are both
/// [`ErrorKind::GrantRefused`], so a caller cannot tell which check failed;
/// an unusable request is [`ErrorKind::InvalidParams`]; a full registry is
/// [`ErrorKind::GrantLimitReached`]; a poisoned lock, a clock error and a
/// failed random source are [`ErrorKind::Internal`]. Each one denies.
/// Test: `resolve_from_sibling_with_valid_token_is_refused`.
fn grant_kind(error: GrantError) -> ErrorKind {
    match error {
        GrantError::Refused | GrantError::Process(_) => ErrorKind::GrantRefused,
        GrantError::InvalidRequest(_) => ErrorKind::InvalidParams,
        GrantError::CapacityReached => ErrorKind::GrantLimitReached,
        GrantError::RegistryPoisoned | GrantError::Clock | GrantError::Random => {
            ErrorKind::Internal
        }
    }
}

/// Decode a whole params object as `T`, discarding the serde error.
fn decode_object<T: DeserializeOwned>(params: Value) -> Result<T, ErrorKind> {
    match params {
        Value::Object(fields) => decode(fields),
        _ => Err(ErrorKind::InvalidParams),
    }
}

/// `secrets.grant`: mint an exec grant for a child and return its token.
///
/// Why: ruling 34 and the Architect's 2026-10-10 ruling — the server mints;
/// any same-uid caller may grant. DOC-74 §15.8 — a registrar under Claude
/// Code may grant only keys flagged "agents may use".
/// What: params are `project` beside [`ExecGrantRequest`]. The registrar is
/// the socket peer; with none, [`ErrorKind::GrantRefused`]. `agent_parent`
/// is [`has_agent_ancestor`] on the registrar's own ancestry, read from the
/// registry's process table; a read error refuses. When it is true, every
/// key must be flagged on the project's backend, else
/// [`ErrorKind::AgentUseRefused`] naming that key in the deny record, before
/// anything is minted. The grant pins the project root and `agent_parent`
/// ([`GrantScope`]). Then the audit log is opened and one allow record per
/// key is written; if either fails, the grant is revoked and the reply is
/// [`ErrorKind::AuditUnavailable`], so no token leaves unaudited.
/// Test: `grant_by_agent_ancestor_cannot_name_unflagged_key`,
/// `grant_by_agent_ancestor_may_name_a_flagged_key`,
/// `grant_records_carry_the_agent_parent_verdict`,
/// `grant_with_unwritable_audit_returns_no_token`.
pub(crate) fn grant(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::Grant,
        Recording::PerKey,
        |gate| {
            let (dir, rest) = split_project(params)?;
            let request: ExecGrantRequest = decode(rest)?;
            // #9070: the registrar is the kernel's peer, never a param.
            let registrar = caller.pid().ok_or(ErrorKind::GrantRefused)?;
            let project = ProjectContext::resolve(state, &dir)?;
            gate.project(&project);
            // #9070: judged server-side on the registrar's ancestry; an
            // unreadable ancestor refuses, it is never skipped.
            let agent_parent = has_agent_ancestor(state.grants.processes(), registrar)
                .map_err(|_| ErrorKind::GrantRefused)?;
            // #9070 slice 3: every grant record says what was judged.
            gate.agent_parent(agent_parent);
            if agent_parent {
                let store = SecretStore::new(project.backend(state)?, state.index.clone());
                for key in &request.keys {
                    let reference = SecretRef::Unscoped { key: key.clone() };
                    if let Err(e) = require_agent_use(&store, project.scopes(), &reference) {
                        gate.key(key);
                        return Err(ErrorKind::from(e));
                    }
                }
            }
            let ttl = Duration::from_secs(request.ttl_secs);
            let mut wanted = GrantRequest::new(request.keys.clone(), request.child_pid, ttl)
                .with_scope(GrantScope::new(project.root(), agent_parent));
            if request.one_shot {
                wanted = wanted.one_shot();
            }
            let minted = state.grants.mint(wanted).map_err(grant_kind)?;
            // #9070: one allow record per key before the token leaves; on any
            // audit failure the grant is removed, so the token is never usable.
            let recorded = gate.admit().and_then(|()| {
                request
                    .keys
                    .iter()
                    .try_for_each(|key| gate.record_key(key, Ok(())))
            });
            if let Err(kind) = recorded {
                // A failed revoke leaves a grant whose token nobody holds.
                let _ = state.grants.revoke(&minted.token);
                return Err(kind);
            }
            to_json(&ExecGrantResponse {
                token: minted.token.expose().to_owned(),
                expires_at: minted.expires_at.as_secs(),
                ttl_secs: minted.ttl.as_secs(),
            })
        },
    )
}

/// `secrets.resolve`: one value, to a caller inside a live grant.
///
/// Why: DOC-74 §15.8 conditions 1-3 and L957 — the only value-returning
/// method. It has no console route and no MCP tool (L991, L1170).
/// What: params are [`ResolveRequest`]. The caller is the socket peer; with
/// none, [`ErrorKind::GrantRefused`]. [`GrantRegistry::check_scoped`]
/// checks token, expiry, key and process tree; every refusal is
/// [`ErrorKind::GrantRefused`]. `agent_parent` is the grant's, or'd with
/// [`has_agent_ancestor`] on the caller's own ancestry (#9070 slice 3); a
/// read error refuses. The key then resolves in the grant's own project,
/// through `resolve_reference` with that `agent_parent`, so the agents flag
/// is checked again before the read. #9621: the allow record is written
/// ahead of the backend read ([`Recording::WriteAhead`]); if the log cannot
/// be opened or the record appended, the reply is
/// [`ErrorKind::AuditUnavailable`], the backend is never opened or read, and
/// no value leaves. A failure after it (the agents flag, the backend, the
/// spend) adds a deny record with its kind. Only after the read is the
/// grant spent ([`GrantRegistry::spend`]); a grant revoked, expired or spent
/// by a racing caller since the check answers [`ErrorKind::GrantRefused`].
/// So a one-shot grant is never spent by a refusal, an audit failure or a
/// backend failure. A refusal before the allow record leaves one deny
/// record, best-effort. #7520: a CLI-backed value goes through the
/// session's cache, so a hit skips the backend read and nothing else.
///
/// [`GrantRegistry::check_scoped`]: super::grant::GrantRegistry::check_scoped
/// [`GrantRegistry::spend`]: super::grant::GrantRegistry::spend
/// Test: `resolve_without_grant_returns_no_value_and_one_deny_record`,
/// `resolve_by_agent_descendant_of_unflagged_grant_is_refused`,
/// `resolve_with_unreadable_caller_ancestry_is_refused`,
/// `resolve_from_sibling_with_valid_token_is_refused`,
/// `resolve_allow_with_unwritable_audit_returns_no_value`,
/// `resolve_with_unwritable_audit_never_reads_the_backend`,
/// `resolve_sentinel_never_reaches_the_audit_file_or_logs`,
/// `one_shot_grant_survives_unreadable_caller_ancestry`,
/// `one_shot_grant_survives_an_audit_failure`,
/// `one_shot_grant_survives_a_backend_failure`,
/// `resolve_that_loses_the_one_shot_race_returns_no_value`,
/// `resolve_against_a_dead_childs_grant_is_refused`,
/// `resolve_second_call_does_not_reread_cli_backend`,
/// `warm_cache_still_refuses_revoked_token`.
pub(crate) fn resolve(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::Resolve,
        Recording::WriteAhead,
        |gate| {
            let request: ResolveRequest = decode_object(params)?;
            gate.key(&request.key);
            // #9070: the peer pid is the kernel's, read by the router.
            let peer = caller.pid().ok_or(ErrorKind::GrantRefused)?;
            let token = GrantToken::from_wire(request.token);
            // #9621: check only; the grant is spent after every later check.
            let scope = state
                .grants
                .check_scoped(&token, slice::from_ref(&request.key), peer)
                .map_err(grant_kind)?;
            // #9070 slice 3: the caller's own ancestry counts too, so a grant
            // a non-agent registered for an agent's pid opens no unflagged
            // key to it. An unreadable ancestor refuses.
            let agent_parent = scope.agent_parent
                || has_agent_ancestor(state.grants.processes(), peer)
                    .map_err(|_| ErrorKind::GrantRefused)?;
            gate.agent_parent(agent_parent);
            let project = ProjectContext::resolve(state, &scope.project)?;
            gate.project(&project);
            // #9621: the allow record before the backend is opened or read,
            // so an audit log that cannot be written refuses with no read;
            // it also precedes the spend, so an audit failure spends nothing.
            gate.write_ahead()?;
            // #7520: every check above ran; a cache hit skips only the read.
            let store = SecretStore::new(project.backend(state)?, state.index.clone())
                .with_cache(Arc::clone(&state.cache));
            let reference = SecretRef::Unscoped {
                key: request.key.clone(),
            };
            let value = resolve_reference(&store, project.scopes(), &reference, agent_parent)?;
            if !state.grants.spend(&token).map_err(grant_kind)? {
                return Err(ErrorKind::GrantRefused);
            }
            to_json(&ResolveResponse {
                key: request.key,
                value,
            })
        },
    )
}

/// `secrets.revoke`: remove the grant a token names.
///
/// Why: tm removes a child's grant when the child exits, so its token stops
/// working before it expires.
/// What: params are [`RevokeRequest`]. The grant is removed first: the
/// fail-closed state is "no grant", so an unwritable audit log never keeps
/// one alive (the reply is then [`ErrorKind::AuditUnavailable`]; the grant
/// is already gone). An unknown token answers
/// `revoked: false`. One record per call.
/// Test: `resolve_sentinel_never_reaches_the_audit_file_or_logs`.
pub(crate) fn revoke(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::Revoke,
        Recording::Once,
        |gate| {
            let request: RevokeRequest = decode_object(params)?;
            let revoked = state
                .grants
                .revoke(&GrantToken::from_wire(request.token))
                .map_err(grant_kind)?;
            gate.admit()?;
            to_json(&RevokeResponse { revoked })
        },
    )
}
