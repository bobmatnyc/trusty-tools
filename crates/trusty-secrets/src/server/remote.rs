//! The server side of `trusty-secrets remote-gate`: `secrets.remote.resolve`
//! and `secrets.remote.doctor` (#11464, epic #11463 phase 1).
//!
//! Why: a remote host's agent reaches this server only through an ssh
//! forced command, `trusty-secrets remote-gate --host <name>`. The socket
//! peer is then the gate, not the agent, so neither grant-by-pid nor the
//! ancestry walk can see the caller. The host is the ssh key's (sshd
//! authenticated it, `authorized_keys` pins `--host`); the agent label the
//! frame carries is the remote side's word and is never trusted.
//! What: both methods
//! 1. verify the socket peer is the gate ([`verify_gate`]): its executable
//!    is `trusty-secrets` and no Claude Code process is above it, so a local
//!    agent cannot use this path to skip a grant;
//! 2. re-check the host's project allowlist in the account's own machine
//!    config (`State::file_consent_config`), never a file the spawner chose;
//! 3. resolve with `agent_parent` fixed `true`, so the agents-may-use deny
//!    check runs exactly as for a local agent.
//!
//! Every refusal in steps 1 and 2 is [`ErrorKind::RemoteRefused`]. Records
//! carry `host` only once the peer was verified, and `remote_agent` always
//! with `verified: false`. `resolve` writes its allow record ahead of the
//! backend read (`Recording::WriteAhead`), so an unwritable log reads
//! nothing.
//! Test: `remote_tests.rs` beside `wire_tests.rs`.

use std::ffi::OsStr;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::ancestry::{ProcessTable, has_agent_ancestor};
use super::audit::{AuditMethod, RemoteAgent};
use super::errors::ErrorKind;
use super::gate::{Gate, Recording, audited};
use super::methods::{Caller, decode};
use super::project::selecting_machine;
use super::router::{State, account_machine};
use crate::api::{BackendId, OwnerName, RepoName, SecretKey, SecretRef};
use crate::store::config::{self, MachineSecretsConfig};
use crate::store::resolve::resolve_reference;
use crate::store::{OpenedUnder, ScopeSet, SecretStore, VaultOverride};

/// The file name of the one executable whose remote calls are served.
///
/// Known limit, as for the console bridge: a same-uid process can run its
/// own binary under this name; the agent-ancestor refusal keeps a local
/// agent from doing so.
pub(crate) const GATE_EXECUTABLE: &str = "trusty-secrets";

/// The longest host name or agent label accepted.
const MAX_LABEL_LEN: usize = 64;

/// A remote call's audit subject: the trusted host, once the gate is
/// verified, the claimed label and the project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RemoteAudit {
    pub(crate) host: Option<String>,
    pub(crate) agent: Option<RemoteAgent>,
    pub(crate) project: Option<String>,
}

/// The params of both remote methods.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteRequest {
    host: String,
    project: String,
    #[serde(default)]
    key: Option<SecretKey>,
    #[serde(default)]
    agent: Option<String>,
}

/// Whether `raw` is a host name or agent label: `[A-Za-z0-9._-]{1,64}`,
/// starting with a letter or digit.
///
/// Why: both reach the audit log, which holds no free text.
/// Test: `remote_label_validation_table`.
pub(crate) fn valid_label(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= MAX_LABEL_LEN
        && raw.starts_with(|c: char| c.is_ascii_alphanumeric())
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Parse `<owner>/<repo>`; anything else is `None`.
///
/// Test: `remote_label_validation_table`.
pub(crate) fn parse_project(raw: &str) -> Option<(OwnerName, RepoName)> {
    let (owner, repo) = raw.split_once('/')?;
    Some((OwnerName::new(owner).ok()?, RepoName::new(repo).ok()?))
}

/// Whether the account config lets `host` reach `<owner>/<repo>`.
///
/// What: the host's entry must exist, not be `enabled: false`, and list the
/// project. Shared by the gate's pre-check and the server's re-check.
/// Test: `remote_allowlist_table`.
pub(crate) fn host_allows(
    machine: &MachineSecretsConfig,
    host: &str,
    owner: &OwnerName,
    repo: &RepoName,
) -> bool {
    machine
        .remote_host(host)
        .is_some_and(|entry| entry.allows(owner, repo))
}

/// The sshd executables an ssh forced command runs under.
const SSHD_EXECUTABLES: [&str; 2] = ["sshd", "sshd-session"];

/// Whether an ancestor of `pid` is a root-owned sshd.
///
/// Why: #11464 critic M1 — a detached local agent (setsid, double fork,
/// launchctl, at) has no Claude Code ancestor left, yet it never came in
/// over ssh. A forced command always runs below sshd.
/// What: walks `pid`'s parents (bounded by `MAX_CHAIN_DEPTH`) for one whose
/// kernel-reported executable is named `sshd` or `sshd-session` and whose
/// file `exe_owner` reports owned by uid 0. A read error is `false`.
/// Test: `remote_resolve_from_a_gate_with_no_sshd_ancestor_is_refused`.
fn has_sshd_ancestor(state: &State, table: &dyn ProcessTable, pid: u32) -> bool {
    let mut current = pid;
    for _ in 0..crate::ancestry::MAX_CHAIN_DEPTH {
        let Ok(parent) = table.parent(current) else {
            return false;
        };
        if parent == current || parent == 0 {
            return false;
        }
        current = parent;
        let Ok(exe) = table.executable(current) else {
            return false;
        };
        let named = exe
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| SSHD_EXECUTABLES.contains(&name));
        if named && (state.exe_owner)(&exe) == Some(0) {
            return true;
        }
        if current <= 1 {
            return false;
        }
    }
    false
}

/// Refuse a peer that is not the gate.
///
/// What: the peer pid must be known, its executable's file name must be
/// [`GATE_EXECUTABLE`], its ancestry must be readable and hold no Claude
/// Code process, and (#11464 critic M1) it must run below a root-owned
/// sshd ([`has_sshd_ancestor`]). Any failure is [`ErrorKind::RemoteRefused`].
/// Test: `remote_resolve_from_a_non_gate_peer_is_refused`,
/// `remote_resolve_from_a_gate_under_an_agent_is_refused`,
/// `remote_resolve_from_a_gate_with_no_sshd_ancestor_is_refused`,
/// `remote_resolve_with_no_peer_pid_is_refused`.
fn verify_gate(state: &State, caller: Caller) -> Result<(), ErrorKind> {
    let pid = caller.pid().ok_or(ErrorKind::RemoteRefused)?;
    let table: &dyn ProcessTable = state.grants.processes();
    let exe = table
        .executable(pid)
        .map_err(|_| ErrorKind::RemoteRefused)?;
    if exe.file_name() != Some(OsStr::new(GATE_EXECUTABLE)) {
        return Err(ErrorKind::RemoteRefused);
    }
    match has_agent_ancestor(table, pid) {
        Ok(false) if has_sshd_ancestor(state, table, pid) => Ok(()),
        _ => Err(ErrorKind::RemoteRefused),
    }
}

/// What an admitted remote call resolves against.
struct Admitted {
    key: Option<SecretKey>,
    scopes: ScopeSet,
    backend: BackendId,
}

/// Decode, verify the peer, re-check the allowlist, and derive the scopes.
///
/// What: an undecodable request, a bad host, label or project is
/// [`ErrorKind::InvalidParams`]; steps 1 and 2 of the module docs are
/// [`ErrorKind::RemoteRefused`]. The scopes are the remote-derived vaults,
/// or the machine `project_vaults` override, as a local call gets them; the
/// backend is the machine `default_backend`, since no tracked file exists.
/// Test: `remote_resolve_of_an_unlisted_project_is_refused_by_the_server`,
/// `remote_resolve_with_an_unreadable_account_config_is_refused`.
fn admit(
    state: &State,
    caller: Caller,
    params: Value,
    gate: &mut Gate<'_>,
) -> Result<Admitted, ErrorKind> {
    let Value::Object(fields) = params else {
        return Err(ErrorKind::InvalidParams);
    };
    let request: RemoteRequest = decode(fields)?;
    if let Some(key) = &request.key {
        gate.key(key);
    }
    let (owner, repo) = parse_project(&request.project).ok_or(ErrorKind::InvalidParams)?;
    let agent = match request.agent.as_deref() {
        Some(label) if valid_label(label) => Some(RemoteAgent::unverified(label)),
        Some(_) => return Err(ErrorKind::InvalidParams),
        None => None,
    };
    let mut subject = RemoteAudit {
        host: None,
        agent,
        project: Some(format!("{owner}/{repo}")),
    };
    gate.remote(subject.clone());
    if !valid_label(&request.host) {
        return Err(ErrorKind::InvalidParams);
    }
    verify_gate(state, caller)?;
    // #11464: trusted from here on: the verified gate's `--host`.
    subject.host = Some(request.host.clone());
    gate.remote(subject);
    // #11464: the account's own file; a missing or unparsable one refuses.
    let account =
        account_machine(state.file_consent_config.as_deref()).ok_or(ErrorKind::RemoteRefused)?;
    if !host_allows(&account, &request.host, &owner, &repo) {
        return Err(ErrorKind::RemoteRefused);
    }
    let machine = selecting_machine(state)?;
    config::check_sync_target_backend(
        None,
        &state.settings.machine_config,
        machine.as_ref(),
        &state.settings.machine_config,
    )?;
    let vault_override = machine
        .as_ref()
        .and_then(|m| m.project_vault(&owner, &repo))
        .cloned()
        .map(VaultOverride::Machine);
    let scopes = ScopeSet::from_identity(&owner, &repo, vault_override)?;
    let backend = config::resolve(None, machine.as_ref()).backend;
    gate.backend(&backend);
    Ok(Admitted {
        key: request.key,
        scopes,
        backend,
    })
}

/// `secrets.remote.resolve`: one value, to the verified gate.
///
/// What: [`admit`], then a `key` is required. `agent_parent` is `true`
/// and the allow record is `default_policy: true`; it is written ahead of
/// the backend open ([`Gate::write_ahead`]), so an unwritable log is
/// [`ErrorKind::AuditUnavailable`] with nothing read. Then the key resolves
/// through `resolve_reference` with `agent_parent` `true`: a denied key is
/// [`ErrorKind::AgentKeyDenied`]. A CLI-backed value goes through the
/// session cache, as for `secrets.resolve`.
/// Test: `remote_resolve_of_an_open_key_returns_it_with_a_remote_record`,
/// `remote_resolve_of_a_denied_key_is_refused`,
/// `remote_resolve_with_unwritable_audit_never_reads_the_backend`.
pub(crate) fn resolve(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::RemoteResolve,
        Recording::WriteAhead,
        |gate| {
            let admitted = admit(state, caller, params, gate)?;
            let key = admitted.key.ok_or(ErrorKind::InvalidParams)?;
            // #11464: every remote read is an agent's read.
            gate.agent_parent(true);
            gate.default_policy();
            gate.write_ahead()?;
            let account = state.file_consent_config.as_deref();
            let before = OpenedUnder::read(account, &admitted.backend);
            let backend = state.open(&admitted.backend)?;
            let opened_under = before.filter(|before| {
                OpenedUnder::read(account, &admitted.backend).as_ref() == Some(before)
            });
            let store = SecretStore::new(backend, state.index.clone())
                .with_deny_audit(gate.deny_audit())
                .with_cache(Arc::clone(&state.cache), opened_under);
            let reference = SecretRef::Unscoped { key: key.clone() };
            let value = resolve_reference(&store, &admitted.scopes, &reference, true)?;
            Ok(json!({"key": key, "value": value}))
        },
    )
}

/// `secrets.remote.doctor`: the remote host's preflight.
///
/// What: [`admit`], then the allow record is written ahead
/// ([`Gate::write_ahead`], #11464 critic M2), then the project's backend is
/// opened (no value is read); an open failure adds a deny record. The reply
/// names the backend and the project vault.
/// Test: `remote_doctor_names_the_backend_and_leaves_one_record`,
/// `remote_doctor_with_unwritable_audit_never_opens_the_backend`.
pub(crate) fn doctor(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::RemoteDoctor,
        Recording::WriteAhead,
        |gate| {
            let admitted = admit(state, caller, params, gate)?;
            // #11464 critic M2: the record before the backend is opened.
            gate.write_ahead()?;
            state.open(&admitted.backend)?;
            Ok(json!({
                "ready": true,
                "backend": admitted.backend,
                "project_vault": admitted.scopes.project(),
            }))
        },
    )
}
