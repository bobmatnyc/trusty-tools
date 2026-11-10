//! `secrets.copy` to a write-only sync target: a push (#9069, DOC-74 §15.4).
//!
//! Why: a push sends a project value outside this machine, to a store any
//! repository writer's workflow can read. So it takes named keys only (D5),
//! opens its target only from the account's own machine config for the
//! checkout's origin repository (D6), and never touches the names-only index
//! (D4). #10999 (owner ruling 2026-11-10, Architect E/F) supersedes D3 for
//! agents: a caller under Claude Code may push a key with no deny, because
//! the value goes from the server to the CLI's stdin and never reaches the
//! caller; a key denied to agents stays un-pushable by one.
//! What: [`push`], the body `methods::copy` hands a sync-target destination
//! to, and [`State::open_sync_target`], the one place a sync target opens.
//! The audit gate is `copy`'s own: one deny record for a refusal before the
//! loop, the log opened before the first key, one record per key.
//! Test: `sync_target_tests.rs` beside `server_tests.rs`.

use std::sync::Arc;

use serde_json::Value;

use super::agents_flag::judge_agent_caller;
use super::errors::ErrorKind;
use super::gate::Gate;
use super::methods::{Caller, to_json};
use super::project::ProjectContext;
use super::router::State;
use crate::api::methods::{CopyRequest, CopyResponse};
use crate::api::{BackendId, SecretsError};
use crate::store::{Capabilities, RemoteIdentity, SecretBackend, SecretStore, open_sync_target_at};

/// Opens a sync target for one repository; replaces the production opener
/// in tests.
pub(crate) type SyncTargetFactory = Arc<
    dyn Fn(&BackendId, &RemoteIdentity) -> Result<Arc<dyn SecretBackend>, SecretsError>
        + Send
        + Sync,
>;

impl State {
    /// Open sync target `id` to push into `identity`'s repository.
    ///
    /// Why: C8, C9 — ruling 74 lets only the account's own machine config
    /// enable a push, and a sandboxed server pushes nowhere.
    /// What: under the sandbox flag, [`SecretsError::BackendNotEnabled`]
    /// before anything opens. Otherwise the test opener when one is set,
    /// else `store::open_sync_target_at` on `State::file_consent_config` —
    /// the account's own file, never `--machine-config` or `$HOME`'s; with
    /// none, [`SecretsError::BackendNotEnabled`]. A push's working directory
    /// goes under `ServerSettings::template_root`, which the startup sweep
    /// clears (#9069 PR 2). Opening spawns nothing.
    /// Test: `server_sandbox_never_opens_a_sync_target`,
    /// `github_push_targets_the_remote_repo_not_the_vault_override`.
    pub(crate) fn open_sync_target(
        &self,
        id: &BackendId,
        identity: &RemoteIdentity,
    ) -> Result<Arc<dyn SecretBackend>, SecretsError> {
        let not_enabled = || SecretsError::BackendNotEnabled {
            backend: id.to_string(),
        };
        // #9069: C9 — refused before any opener, as `State::open` refuses.
        if self.sandboxed {
            return Err(not_enabled());
        }
        if let Some(open) = &self.sync_targets {
            return open(id, identity);
        }
        let config = self
            .file_consent_config
            .as_deref()
            .ok_or_else(not_enabled)?;
        open_sync_target_at(id, config, identity, &self.settings.template_root)
    }
}

/// Push `request.keys` from `request.from_backend` to the sync target
/// `request.to_backend`, inside `methods::copy`'s audited body.
///
/// Why: DOC-74 §15.4 "Push". #10999 supersedes D3 for agent callers: the
/// value never reaches the caller, so an agent may push what it may use.
/// What, in order, each refusal before the loop leaving `copy`'s one deny
/// record: an empty key list is [`ErrorKind::InvalidParams`] (D5); no peer
/// pid is [`ErrorKind::GrantRefused`]; the caller's ancestry, or on a bridge
/// call the console's verdict, is judged ([`judge_agent_caller`]) and an
/// unreadable one is [`ErrorKind::GrantRefused`]; the source opens through
/// [`State::open`] and the target through [`State::open_sync_target`] with
/// the project's origin identity; a source without `READ` or a target
/// without `WRITE | SYNC_TARGET` is [`ErrorKind::Unsupported`]. Then the
/// audit log is opened, and each key is read from the project vault and
/// written straight to the target: no index row and no compensation (D4).
/// For a caller under Claude Code, a key denied to agents in the source, or
/// one whose deny cannot be read, fails with [`ErrorKind::AgentKeyDenied`]
/// (or the lookup's kind) before its value is read; its allow records say
/// `default_policy`. Every record names the caller's pid and agent verdict,
/// never a value.
/// A failed key lands in `failed` with a deny record and the next key runs;
/// no key starts after the request deadline; a record that cannot be
/// written stops the push with [`ErrorKind::AuditUnavailable`].
/// Test: `server_copy_to_sync_target_without_keys_is_invalid_params`,
/// `server_copy_to_sync_target_by_agent_descendant_pushes_an_undenied_key`,
/// `agent_push_of_a_denied_key_fails_it_before_its_value_is_read`,
/// `agent_github_push_keeps_the_value_out_of_argv_env_reply_and_audit`,
/// `agent_vercel_push_keeps_the_value_out_of_argv_env_reply_and_audit`,
/// `server_copy_to_sync_target_with_unreadable_ancestry_is_refused`,
/// `bridge_copy_to_sync_target_uses_the_console_verdict`,
/// `server_copy_to_sync_target_leaves_one_record_per_key_naming_the_target`,
/// `server_copy_to_sync_target_with_unwritable_audit_spawns_nothing`,
/// `server_copy_to_sync_target_leaves_the_index_untouched`.
pub(crate) fn push(
    state: &State,
    caller: Caller,
    gate: &mut Gate<'_>,
    request: &CopyRequest,
    project: &ProjectContext,
) -> Result<Value, ErrorKind> {
    // #9069: D5 — "every key" would publish the whole project.
    if request.keys.is_empty() {
        return Err(ErrorKind::InvalidParams);
    }
    // #9069: the caller is the kernel's peer, never a param.
    let peer = caller.pid().ok_or(ErrorKind::GrantRefused)?;
    // #10999: E supersedes D3 for agents; an unreadable chain still refuses.
    let agent = judge_agent_caller(state, caller, peer, gate)?;
    if agent {
        gate.default_policy();
    }
    let source = state.open(&request.from_backend)?;
    let target = state.open_sync_target(&request.to_backend, project.identity())?;
    if !source.capabilities().contains(Capabilities::READ)
        || !target
            .capabilities()
            .contains(Capabilities::WRITE | Capabilities::SYNC_TARGET)
    {
        return Err(ErrorKind::Unsupported);
    }
    let vault = project.scopes().project().clone();
    // #10999: F — for an agent, the source's deny is read before any value.
    let source_store = SecretStore::new(Arc::clone(&source), state.index.clone())
        .with_deny_audit(gate.deny_audit());
    let mut response = CopyResponse {
        copied: Vec::new(),
        failed: Vec::new(),
    };
    gate.admit()?;
    for key in &request.keys {
        gate.ready()?;
        // #7524 P2-M1: no key starts after the deadline.
        let outcome = if crate::store::deadline::passed() {
            Err(ErrorKind::DeadlineExceeded)
        } else {
            // #10999: F — a denied key, or a failed deny lookup, fails the key.
            let admitted = match agent {
                true => match source_store.agents_may_use(&vault, key) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(ErrorKind::AgentKeyDenied),
                    Err(e) => Err(ErrorKind::from(e)),
                },
                false => Ok(()),
            };
            admitted.and_then(|()| match source.get(&vault, key) {
                // #9069: D4 — straight to the target; the index describes local values.
                Ok(Some(value)) => target.set(&vault, key, &value).map_err(ErrorKind::from),
                Ok(None) => Err(ErrorKind::NotFound),
                Err(e) => Err(ErrorKind::from(e)),
            })
        };
        gate.record_key(key, outcome)?;
        match outcome {
            Ok(()) => response.copied.push(key.clone()),
            Err(_) => response.failed.push(key.clone()),
        }
    }
    to_json(&response)
}
