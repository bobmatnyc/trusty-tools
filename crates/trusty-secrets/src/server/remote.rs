//! `secrets.gate`: the one server entry `trusty-secrets remote-gate` uses
//! (#11464, owner ruling D3a).
//!
//! Why: a remote host's agent reaches this server only through an ssh
//! forced command, `trusty-secrets remote-gate --host <name>`. The socket
//! peer is then the gate under sshd, which the ancestry walk judges a
//! person, so `secrets.resolve` would skip the agents-may-use deny check.
//! D3a: one narrow entry for that gate only, with the deny list applied.
//! What: [`gate`] serves a closed enum of two inner verbs, `resolve` and
//! `doctor`; any other verb, or any unknown field, is
//! [`ErrorKind::InvalidParams`]. It never serves `set`, `delete`, a grant
//! or a list. Before it decodes anything it verifies the peer
//! ([`verify_gate`]); a failure is [`ErrorKind::GateRefused`] and one
//! best-effort deny record ([`Recording::DenyOnly`], as `bridged` does).
//! - `resolve` (critic M-A) re-checks the host's allowlist in the account's
//!   own machine config ([`ErrorKind::RemoteRefused`]), then runs the
//!   `secrets.resolve` body unchanged ([`exec::resolve_with`]): the grant
//!   token, `check_scoped` and the write-ahead record all still apply. The
//!   one difference is `agent_parent`, forced `true` by this server, never
//!   read from the request. The records carry `host`, the unverified
//!   agent label and the project.
//! - `doctor` runs the `secrets.doctor` body, audited as it is today.
//!
//! Test: `remote_tests.rs` beside `wire_tests.rs`.

use std::ffi::OsStr;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::ancestry::{ProcessTable, has_agent_ancestor};
use super::audit::{AuditMethod, RemoteAgent};
use super::errors::ErrorKind;
use super::exec;
use super::gate::{Recording, audited};
use super::methods::{Caller, decode};
use super::router::{State, account_machine};
use crate::api::{OwnerName, RepoName, SecretKey};
use crate::store::config::MachineSecretsConfig;

/// The file name of the one executable whose calls are served.
///
/// Known limit, as for the console bridge: a same-uid process can run its
/// own binary under this name; the agent-ancestor and sshd checks of
/// [`verify_gate`] narrow who can.
pub(crate) const GATE_EXECUTABLE: &str = "trusty-secrets";

/// The kernel short names of the sshd an ssh forced command runs under;
/// OpenSSH 9.8+ runs a session as `sshd-session` (critic M-B).
const SSHD_NAMES: [&str; 2] = ["sshd", "sshd-session"];

/// The longest host name or agent label accepted.
const MAX_LABEL_LEN: usize = 64;

/// A gate call's audit subject: the host, the claimed label and the project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RemoteAudit {
    pub(crate) host: Option<String>,
    pub(crate) agent: Option<RemoteAgent>,
    pub(crate) project: Option<String>,
}

/// The params of `secrets.gate`: one of two inner verbs.
///
/// What: tagged by `verb`; unknown verbs and unknown fields (an
/// `agent_parent` among them) do not decode.
#[derive(Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case", deny_unknown_fields)]
enum GateRequest {
    /// `secrets.resolve`'s params, plus the gate's subject.
    Resolve {
        token: String,
        key: SecretKey,
        host: String,
        project: String,
        #[serde(default)]
        agent: Option<String>,
    },
    /// `secrets.doctor`, with no params.
    Doctor {},
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

/// Whether the account config lets `host` resolve `key` of `<owner>/<repo>`.
///
/// What: the host's entry must exist, not be `enabled: false`, map the
/// project and list the key. Shared by the gate's pre-check and the
/// server's re-check.
/// Test: `remote_allowlist_table`.
pub(crate) fn host_allows(
    machine: &MachineSecretsConfig,
    host: &str,
    owner: &OwnerName,
    repo: &RepoName,
    key: &SecretKey,
) -> bool {
    machine
        .remote_host(host)
        .is_some_and(|entry| entry.project_dir(owner, repo).is_some() && entry.allows_key(key))
}

/// Whether an ancestor of `pid` is an sshd owned by root.
///
/// Why: #11464 critic M1 — a detached local process has no Claude Code
/// ancestor left, yet never came in over ssh; a forced command always runs
/// below sshd. Critic M-B: the root sshd's path is unreadable to this uid
/// on macOS, so its kernel short name is read ([`ProcessTable::comm`]).
/// What: walks `pid`'s parents, bounded by `MAX_CHAIN_DEPTH`. `Ok(true)` at
/// the first one named in [`SSHD_NAMES`] with uid 0; `Ok(false)` at pid 0
/// or 1 or a self-parented pid. Every unreadable step is an `Err`, and a
/// chain too long is `Err` too, so the caller refuses.
/// Test: `gate_from_a_peer_with_no_sshd_ancestor_is_refused`,
/// `gate_with_unreadable_ancestry_is_refused`.
fn has_sshd_ancestor(table: &dyn ProcessTable, pid: u32) -> Result<bool, ()> {
    let mut current = pid;
    for _ in 0..crate::ancestry::MAX_CHAIN_DEPTH {
        let parent = table.parent(current).map_err(|_| ())?;
        if parent == current || parent <= 1 {
            return Ok(false);
        }
        current = parent;
        let comm = table.comm(current).map_err(|_| ())?;
        if SSHD_NAMES.contains(&comm.name.as_str()) && comm.uid == 0 {
            return Ok(true);
        }
    }
    Err(())
}

/// Refuse a peer that is not the gate.
///
/// What: the peer pid must be known, its executable's file name must be
/// [`GATE_EXECUTABLE`], its ancestry must be readable and hold no Claude
/// Code process, and it must run below a root sshd ([`has_sshd_ancestor`]).
/// Any failure, unreadable step included, is [`ErrorKind::GateRefused`];
/// there is no fall-back to the local-caller path.
/// Test: `gate_from_a_non_gate_peer_is_refused` (other executable, Claude
/// Code ancestor), `gate_from_a_peer_with_no_sshd_ancestor_is_refused`,
/// `gate_with_unreadable_ancestry_is_refused`,
/// `gate_with_no_peer_pid_is_refused`.
fn verify_gate(state: &State, caller: Caller) -> Result<(), ErrorKind> {
    let refused = ErrorKind::GateRefused;
    let pid = caller.pid().ok_or(refused)?;
    let table: &dyn ProcessTable = state.grants.processes();
    let exe = table.executable(pid).map_err(|_| refused)?;
    if exe.file_name() != Some(OsStr::new(GATE_EXECUTABLE)) {
        return Err(refused);
    }
    match (
        has_agent_ancestor(table, pid),
        has_sshd_ancestor(table, pid),
    ) {
        (Ok(false), Ok(true)) => Ok(()),
        _ => Err(refused),
    }
}

/// `secrets.gate`: the remote gate's `resolve` or `doctor`.
///
/// What: see the module docs. Identity first: a peer that fails
/// [`verify_gate`] is refused whatever it sent.
/// Test: `gate_resolve_of_an_open_key_returns_it_with_a_remote_record`,
/// `gate_resolve_of_a_denied_key_is_refused`,
/// `gate_from_a_non_gate_peer_is_refused`,
/// `gate_unknown_inner_verb_is_refused`,
/// `gate_request_carrying_agent_parent_is_refused`.
pub(crate) fn gate(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    let Value::Object(fields) = params else {
        return refuse(state, caller, &Map::new(), ErrorKind::InvalidParams);
    };
    if let Err(kind) = verify_gate(state, caller) {
        return refuse(state, caller, &fields, kind);
    }
    match decode::<GateRequest>(fields)? {
        GateRequest::Resolve {
            token,
            key,
            host,
            project,
            agent,
        } => {
            let agent = match agent.as_deref() {
                Some(label) if valid_label(label) => Some(RemoteAgent::unverified(label)),
                Some(_) => return Err(ErrorKind::InvalidParams),
                None => None,
            };
            let (owner, repo) = parse_project(&project).ok_or(ErrorKind::InvalidParams)?;
            if !valid_label(&host) {
                return Err(ErrorKind::InvalidParams);
            }
            let subject = RemoteAudit {
                host: Some(host.clone()),
                agent,
                project: Some(format!("{owner}/{repo}")),
            };
            // #11464: the account's own file; a missing, unparsable or
            // non-listing one refuses.
            let allowed = account_machine(state.file_consent_config.as_deref())
                .is_some_and(|account| host_allows(&account, &host, &owner, &repo, &key));
            if !allowed {
                return audited(
                    state,
                    caller,
                    AuditMethod::Resolve,
                    Recording::DenyOnly,
                    |g| {
                        g.key(&key);
                        g.remote(subject);
                        Err(ErrorKind::RemoteRefused)
                    },
                );
            }
            let params = json!({"token": token, "key": key});
            exec::resolve_with(state, caller, params, Some(subject))
        }
        GateRequest::Doctor {} => super::doctor::doctor(state, caller, Value::Null),
    }
}

/// One best-effort deny record for a refused gate call, then `kind`.
///
/// What: #11464 critic LOW — as `bridged` does for `BridgeRefused`, a
/// `secrets.resolve` deny ([`Recording::DenyOnly`]) naming the key the
/// params carried, when it decodes.
fn refuse(
    state: &State,
    caller: Caller,
    fields: &Map<String, Value>,
    kind: ErrorKind,
) -> Result<Value, ErrorKind> {
    let key = fields
        .get("key")
        .and_then(|raw| SecretKey::deserialize(raw).ok());
    audited(
        state,
        caller,
        AuditMethod::Resolve,
        Recording::DenyOnly,
        |g| {
            if let Some(key) = &key {
                g.key(key);
            }
            Err(kind)
        },
    )
}
