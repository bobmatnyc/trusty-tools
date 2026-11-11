//! `secrets.copy` with a named source: one key from another store (#10998).
//!
//! Why: owner ruling 2026-11-10 22:22Z — "the user should be able to point a
//! secret at another store … copying a secret between one another is a user
//! allowable action, does not require agents to look at the secret." The
//! Architect's v1 decisions: a copy, not a reference; sources are any
//! Keychain item and any trusty vault key, the owner vault included, so an
//! owner-vault key may land in this project (relaxes DOC-74 §13 Q6); the
//! per-key agent deny (#10386, #10459) still binds agent callers; one audit
//! record per key, never the value.
//! What: [`copy_named`] runs inside `methods::copy`'s audited body when the
//! request carries `source`. The value moves in this process only: read
//! from the source, written through [`SecretStore`] into this project's
//! vault on `to_backend`, never returned, logged or recorded. Each record
//! of the call carries [`CopySourceAudit`].
//! Test: `copy_source_tests.rs` beside this file (a child of `wire_tests`).
//!
//! # Spec References
//! - DOC-74 §9, §13 Q6, §15.2 (`docs/specs/DOC-74-secrets-integration.md`)

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::agents_flag::{Writer, judge_agent_caller, writer_for};
use super::errors::ErrorKind;
use super::gate::Gate;
use super::methods::{Caller, to_json};
use super::project::ProjectContext;
use super::router::State;
use super::ticket::TicketOp;
use super::ticket_checks::TicketCall;
use crate::api::methods::{CopyRequest, CopyResponse, CopySource, KeychainLabel};
use crate::api::{BackendId, SecretKey, VAULT_PREFIX, VaultName};
use crate::store::keychain_item::read_keychain_item;
use crate::store::{Capabilities, SecretBackend, SecretStore};

/// A named copy's source, as each of its audit records names it (#10998).
///
/// Why: the Architect's audit ruling — a record names the source locator,
/// the destination, the key and the caller, never the value.
/// What: the backend read and the store-native name (a Keychain service and
/// account, or a trusty vault and key). The destination is the record's
/// own `backend`, `vault` and `key`.
/// Test: `copy_named_keychain_item_never_leaks_its_value`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CopySourceAudit {
    /// The backend the value was read from.
    pub backend: BackendId,
    /// The name of the value in that backend.
    pub locator: CopySource,
}

/// Copy one key from `source` into this project's vault (#10998).
///
/// What, in order, each refusal leaving `copy`'s one deny record:
/// `keys` not exactly one name, a Keychain item read from a backend other
/// than `keychain`, or a Keychain service in trusty's own namespace
/// ([`in_trusty_namespace`]) is [`ErrorKind::InvalidParams`]; a sync target
/// on either side is [`ErrorKind::Unsupported`] (v1 copies locally only);
/// then the project resolves; a vault source that is the destination
/// itself is [`ErrorKind::SameBackend`]; no peer pid is
/// [`ErrorKind::GrantRefused`]; the caller is judged and its [`Writer`]
/// chosen as in a local copy; the destination's posture check, both opens
/// (the source through [`State::open`], so a sandboxed server refuses the
/// Keychain) and their `READ`/`WRITE` capabilities. Then the audit log is
/// opened and the key moved by [`move_one`]. Its outcome is the key's one
/// record. A failed key is the call's error — never a success reply with a
/// `failed` list — so the caller sees the typed kind.
/// Test: `copy_named_keychain_item_never_leaks_its_value`,
/// `copy_named_owner_vault_key_lands_under_a_new_name`,
/// `copy_named_refusals_are_typed_and_write_nothing`,
/// `copy_named_by_agent_of_a_denied_source_or_destination_is_refused`,
/// `copy_named_by_agent_of_a_keychain_item_lands_denied`,
/// `copy_named_carries_a_vault_source_deny_to_the_destination_key`,
/// `copy_named_with_unwritable_audit_writes_nothing`.
pub(crate) fn copy_named(
    state: &State,
    caller: Caller,
    gate: &mut Gate<'_>,
    dir: PathBuf,
    request: &CopyRequest,
    source: &CopySource,
) -> Result<Value, ErrorKind> {
    gate.copy_source(CopySourceAudit {
        backend: request.from_backend.clone(),
        locator: source.clone(),
    });
    let [key] = request.keys.as_slice() else {
        return Err(ErrorKind::InvalidParams);
    };
    check_locator(&request.from_backend, source)?;
    // #10998: v1 is a local copy; a push still reads the project vault.
    if request.from_backend.is_sync_target() || request.to_backend.is_sync_target() {
        return Err(ErrorKind::Unsupported);
    }
    let project = ProjectContext::resolve(state, &dir)?;
    gate.project(&project);
    let vault = project.scopes().project().clone();
    gate.name(&vault, Some(key));
    if let CopySource::Vault {
        vault: from_vault,
        key: from_key,
    } = source
        && request.from_backend == request.to_backend
        && *from_vault == vault
        && from_key == key
    {
        return Err(ErrorKind::SameBackend);
    }
    let peer = caller.pid().ok_or(ErrorKind::GrantRefused)?;
    let agent = judge_agent_caller(state, caller, peer, gate)?;
    let call = TicketCall {
        op: TicketOp::Copy,
        project: project.root(),
        key: Some(key),
    };
    let writer = writer_for(state, caller, peer, gate, agent, &call)?;
    if writer == Writer::Agent {
        gate.default_policy();
    }
    let destination = project.open_for_write(state, &request.to_backend)?;
    let from = state.open(&request.from_backend)?;
    if !from.capabilities().contains(Capabilities::READ)
        || !destination.capabilities().contains(Capabilities::WRITE)
    {
        return Err(ErrorKind::Unsupported);
    }
    let store =
        SecretStore::new(destination, state.index.clone()).with_deny_audit(gate.deny_audit());
    let source_store =
        SecretStore::new(Arc::clone(&from), state.index.clone()).with_deny_audit(gate.deny_audit());
    gate.admit()?;
    gate.ready()?;
    let outcome = if crate::store::deadline::passed() {
        Err(ErrorKind::DeadlineExceeded)
    } else {
        let moved = move_one(
            state,
            source,
            Ends {
                from: from.as_ref(),
                source_store: &source_store,
                store: &store,
            },
            (&vault, key),
            writer,
        );
        // #7520: the destination may now hold a new value.
        state.cache.evict(&vault, key);
        moved
    };
    gate.record_key(key, outcome)?;
    outcome?;
    to_json(&CopyResponse {
        copied: vec![key.clone()],
        failed: Vec::new(),
    })
}

/// Refuse a locator its backend cannot hold, or one that hides a trusty key.
///
/// What: a Keychain item needs `from_backend` `keychain`, and its service
/// outside trusty's namespace; either failure is
/// [`ErrorKind::InvalidParams`]. A vault key is read from any backend.
fn check_locator(from_backend: &BackendId, source: &CopySource) -> Result<(), ErrorKind> {
    match source {
        CopySource::KeychainItem { service, .. } => {
            if from_backend.as_str() != BackendId::KEYCHAIN || in_trusty_namespace(service) {
                return Err(ErrorKind::InvalidParams);
            }
            Ok(())
        }
        CopySource::Vault { .. } => Ok(()),
    }
}

/// Whether a Keychain service is in trusty's namespace: any service starting
/// `trusty`, case-insensitively — a vault (`trusty/...`), a control item
/// (`trusty-secrets.*`), or any other `trusty…` name.
///
/// Why: #10998 — a trusty key named as a raw Keychain item would skip its
/// agent deny, and a control item is no secret. Such a key is named as a
/// `vault` source instead. Folded to lowercase, so a case variant of a
/// vault name is refused too (fail closed).
/// Test: `copy_named_refusals_are_typed_and_write_nothing`.
fn in_trusty_namespace(service: &KeychainLabel) -> bool {
    service
        .as_str()
        .to_ascii_lowercase()
        .starts_with(VAULT_PREFIX)
}

/// The two stores one key moves between.
struct Ends<'a> {
    /// The source backend.
    from: &'a dyn SecretBackend,
    /// The source backend's store, for its agent deny.
    source_store: &'a SecretStore,
    /// The destination store.
    store: &'a SecretStore,
}

/// Move one value from `source` to `to` in the destination store.
///
/// What: for [`Writer::Agent`], a vault source's deny, then the
/// destination key's deny, is [`ErrorKind::AgentKeyDenied`] before any value
/// is read; a failed lookup is its error. A vault source's deny is carried
/// to the destination key before the value (`carry_deny_between`). If the
/// read then misses or fails, that carried deny stays on a destination key
/// with no value: it fails closed, and a person lifts it with
/// `set_agents_may_use`. The value is read from the vault, or from the item
/// through `state.keychain_item_read` under the Keychain time limit; a miss
/// is [`ErrorKind::NotFound`]. It is written with `set_for_agent` or `set`,
/// and dropped here. #10998 fix round 1: an agent's copy of a Keychain item
/// lands denied — see [`agent_item_write`].
/// Test: `copy_named_by_agent_of_a_keychain_item_lands_denied`.
fn move_one(
    state: &State,
    source: &CopySource,
    ends: Ends<'_>,
    to: (&VaultName, &SecretKey),
    writer: Writer,
) -> Result<(), ErrorKind> {
    let (vault, key) = to;
    if writer == Writer::Agent {
        if let CopySource::Vault {
            vault: from_vault,
            key: from_key,
        } = source
            && !ends.source_store.agents_may_use(from_vault, from_key)?
        {
            return Err(ErrorKind::AgentKeyDenied);
        }
        // #10459: the authoritative check is `set_for_agent`'s, under the
        // index lock; this one keeps a denied destination from a read.
        if !ends.store.agents_may_use(vault, key)? {
            return Err(ErrorKind::AgentKeyDenied);
        }
    }
    let value = match source {
        CopySource::Vault {
            vault: from_vault,
            key: from_key,
        } => {
            // #10386: the deny lands before the value, or nothing does.
            ends.store
                .carry_deny_between(ends.source_store, (from_vault, from_key), to)?;
            ends.from.get(from_vault, from_key)?
        }
        CopySource::KeychainItem { service, account } => {
            read_keychain_item(service, account, state.keychain_item_read)?
        }
    };
    let value = value.ok_or(ErrorKind::NotFound)?;
    let written = match (writer, source) {
        (Writer::Agent, CopySource::KeychainItem { .. }) => {
            return agent_item_write(ends.store, to, &value);
        }
        (Writer::Agent, CopySource::Vault { .. }) => ends.store.set_for_agent(vault, key, &value),
        (Writer::Architect | Writer::Person, _) => ends.store.set(vault, key, &value),
    };
    written.map(drop).map_err(ErrorKind::from)
}

/// Write an agent's copy of a Keychain item under an agent deny (#10998).
///
/// Why: code-critic HIGH on f11dc5c68f — an item the agent copied landed
/// open, so the same agent could grant and resolve it, or pipe it through
/// `tm secrets exec`, and read any login-Keychain item; the exec scrubber
/// redacts only the literal value. A copy is the user's action, not a
/// release to agents.
/// What: the destination key's deny is written first (recorded, through
/// [`SecretStore::write_deny`]); a destination that cannot hold a deny is
/// [`ErrorKind::Unsupported`] and nothing is written. Then the value is
/// written with `set`, since `set_for_agent` would refuse the key it just
/// denied; the destination was checked open before the read. A person lifts
/// the deny with `set_agents_may_use`.
/// Test: `copy_named_by_agent_of_a_keychain_item_lands_denied`.
fn agent_item_write(
    store: &SecretStore,
    to: (&VaultName, &SecretKey),
    value: &crate::api::SecretValue,
) -> Result<(), ErrorKind> {
    store.write_deny(to.0, to.1)?;
    store
        .set(to.0, to.1, value)
        .map(drop)
        .map_err(ErrorKind::from)
}
