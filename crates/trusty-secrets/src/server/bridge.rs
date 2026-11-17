//! The console bridge's verdict: `secrets.bridge.*` (DOC-74 §15.6, S3a,
//! #9066).
//!
//! Why: Dd7 option B (owner, 2026-10-11) — a call the console forwards gets
//! parity with the direct socket. The socket's peer is the console, never
//! the HTTP client it serves, so the direct ancestry rule cannot see an
//! agent's `curl`. The console maps the client to a pid, judges that pid's
//! chain, and asserts the verdict; this module decides whether to believe
//! it, then runs the direct method body under it.
//! What: [`METHODS`], one bridged entry per method the console forwards
//! (`backends`, `scopes`, `list`, `set`, `delete`, `copy`,
//! `set_agents_may_use`); `backends` (#9067 P1) runs a bridge-only body,
//! the rest their direct method. Each entry:
//! 1. takes the [`BRIDGE_FIELD`] assertion out of the params; one that is
//!    missing, does not decode, carries an unknown field or contradicts its
//!    source is `malformed`;
//! 2. verifies the socket peer is the console ([`verify_console`]); a peer
//!    that is not is [`ErrorKind::BridgeRefused`];
//! 3. refuses a malformed assertion with [`ErrorKind::InvalidParams`];
//! 4. otherwise runs the direct body with the verdict on its [`Caller`], so
//!    every record it leaves carries `source` and `agent_parent`, and
//!    `set_agents_may_use` takes the console's verdict in place of its own
//!    ancestry walk.
//!
//! Every bridge call is audited. An allowed change keeps its direct
//! method's fail-closed audit; a bridged `list`, which leaves no allow
//! record when direct, leaves one and is admitted fail-closed like `set`
//! (see `methods::list`). So an unwritable log refuses every allowed bridge
//! call with `audit_unavailable` and changes or lists nothing. Each refusal
//! in steps 2 and 3 leaves one deny record, best-effort like every other
//! refusal, naming the vault and key the params carried when they decode.
//! Test: `bridge_tests.rs` beside `wire_tests.rs`.

use std::ffi::OsStr;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::agents_flag;
use super::ancestry::{ProcessTable, has_agent_ancestor};
use super::audit::{AuditMethod, AuditSource};
use super::errors::ErrorKind;
use super::gate::{Gate, Recording, audited};
use super::methods::{self, Caller, MethodFn};
use super::router::State;
use crate::api::bridge::method as bridged;
use crate::api::{BRIDGE_FIELD, BridgeAssertion, SecretKey, VaultName};

/// The file name of the one executable whose assertion is believed.
///
/// Why: the socket peer's executable is the identity the kernel already
/// exposes for the peer pid the router reads; a launch credential would
/// need a secret shared with a server that any client may spawn first.
/// Known limit: a same-uid process can run its own binary under this name;
/// [`verify_console`] also refuses a "console" with Claude Code above it, so
/// an agent cannot simply start one.
pub(crate) const CONSOLE_EXECUTABLE: &str = "trusty-console";

/// Every bridged method, with its entry.
pub(crate) const METHODS: [(&str, MethodFn); 7] = [
    // #9067 P1: the page's copy dialog; bridge-only.
    (bridged::BACKENDS, backends),
    // #9067: the page's scope picker.
    (bridged::SCOPES, scopes),
    (bridged::LIST, list),
    (bridged::SET, set),
    (bridged::DELETE, delete),
    (bridged::COPY, copy),
    (bridged::SET_AGENTS_MAY_USE, set_agents_may_use),
];

/// The console's verdict on the client a bridge call serves.
///
/// What: the asserted `agent_parent` and its source, or no `agent_parent`
/// and [`AuditSource::Malformed`] for an assertion the server refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BridgeVerdict {
    agent_parent: Option<bool>,
    source: AuditSource,
}

impl BridgeVerdict {
    /// The verdict a consistent `assertion` states.
    fn asserted(assertion: BridgeAssertion) -> Self {
        Self {
            agent_parent: Some(assertion.agent_parent),
            source: assertion.source.into(),
        }
    }

    /// A verdict as the verified console would state it; tests only (#10459).
    #[cfg(test)]
    pub(crate) fn mapped(agent_parent: bool) -> Self {
        Self {
            agent_parent: Some(agent_parent),
            source: AuditSource::Mapped,
        }
    }

    /// The verdict of an assertion the server refused.
    fn malformed() -> Self {
        Self {
            agent_parent: None,
            source: AuditSource::Malformed,
        }
    }

    /// The asserted `agent_parent`, for the audit record.
    pub(crate) fn agent_parent(self) -> Option<bool> {
        self.agent_parent
    }

    /// Where the verdict came from, for the audit record.
    pub(crate) fn source(self) -> AuditSource {
        self.source
    }

    /// The ancestry answer the direct socket would have reached.
    ///
    /// Why: Architect 04:03Z — a mapped pid whose chain cannot be read gets
    /// what the direct socket gives an unreadable chain: `has_agent_ancestor`
    /// errs, and `set_agents_may_use` refuses ON with
    /// [`ErrorKind::GrantRefused`]. #9067 Architect Q5: `unmappable` is an
    /// agent, so ON is [`ErrorKind::AgentUseRefused`].
    /// What: `mapped` answers its boolean, `unmappable` `true`;
    /// `unreadable` is [`ErrorKind::GrantRefused`]; anything else (never
    /// reached past [`bridged`]) is [`ErrorKind::InvalidParams`].
    /// Test: `bridge_verdict_parity_with_direct_socket_table`,
    /// `bridge_unreadable_chain_matches_direct_socket`.
    pub(crate) fn judged(self) -> Result<bool, ErrorKind> {
        match (self.source, self.agent_parent) {
            (AuditSource::Mapped, Some(agent_parent)) => Ok(agent_parent),
            // #9067: fail closed; an unmappable client is never a person.
            (AuditSource::Unmappable, Some(true)) => Ok(true),
            (AuditSource::Unreadable, _) => Err(ErrorKind::GrantRefused),
            _ => Err(ErrorKind::InvalidParams),
        }
    }
}

fn scopes(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    bridged(state, caller, params, AuditMethod::Scopes, methods::scopes)
}

fn backends(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    bridged(
        state,
        caller,
        params,
        AuditMethod::Backends,
        super::doctor::backends,
    )
}

fn list(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    bridged(state, caller, params, AuditMethod::List, methods::list)
}

fn set(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    bridged(state, caller, params, AuditMethod::Set, methods::set)
}

fn delete(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    bridged(state, caller, params, AuditMethod::Delete, methods::delete)
}

fn copy(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    bridged(state, caller, params, AuditMethod::Copy, methods::copy)
}

fn set_agents_may_use(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    bridged(
        state,
        caller,
        params,
        AuditMethod::SetAgentsMayUse,
        agents_flag::set_agents_may_use,
    )
}

/// Check the assertion and the peer, then run `body` under the verdict.
///
/// What: steps 1–4 of the module docs. A refusal is one deny record for
/// `method`, carrying the verdict's `source` and `agent_parent`.
/// Test: `bridge_assertion_from_non_console_peer_is_refused`,
/// `bridge_console_with_unreadable_chain_is_refused`,
/// `bridge_malformed_assertion_is_refused_and_audited`,
/// `bridge_with_no_peer_pid_is_refused`,
/// `bridge_call_is_audited_allow_and_deny`.
fn bridged(
    state: &State,
    caller: Caller,
    params: Value,
    method: AuditMethod,
    body: MethodFn,
) -> Result<Value, ErrorKind> {
    let (verdict, rest) = split_assertion(params);
    // #9066: the verdict rides on the caller, so every record names it.
    let caller = caller.with_bridge(verdict);
    // #9066: identity first; a non-console peer is refused whatever it sent.
    let checked = verify_console(state, caller).and_then(|()| match verdict.source() {
        AuditSource::Malformed => Err(ErrorKind::InvalidParams),
        _ => Ok(()),
    });
    match checked {
        Ok(()) => body(state, caller, Value::Object(rest)),
        Err(kind) => audited(state, caller, method, Recording::DenyOnly, |gate| {
            name_attempted(gate, &rest);
            Err(kind)
        }),
    }
}

/// Name on the deny record the vault and key a refused call attempted.
///
/// What: best-effort — a `vault` or `key` that is missing or does not
/// decode is left off; the refusal stands either way (#9066).
/// Test: `bridge_assertion_from_non_console_peer_is_refused`,
/// `bridge_malformed_assertion_is_refused_and_audited`.
fn name_attempted(gate: &mut Gate<'_>, fields: &Map<String, Value>) {
    let key = field::<SecretKey>(fields, "key");
    match field::<VaultName>(fields, "vault") {
        Some(vault) => gate.name(&vault, key.as_ref()),
        None => {
            if let Some(key) = &key {
                gate.key(key);
            }
        }
    }
}

/// The field `name` of `fields` as a `T`, or `None`.
fn field<T: DeserializeOwned>(fields: &Map<String, Value>, name: &str) -> Option<T> {
    T::deserialize(fields.get(name)?).ok()
}

/// Take the [`BRIDGE_FIELD`] assertion out of `params`.
///
/// What: the verdict and the remaining fields. Params that are not an
/// object, or an assertion that is missing, does not decode (unknown fields
/// included) or is not [`BridgeAssertion::is_consistent`], give a malformed
/// verdict.
fn split_assertion(params: Value) -> (BridgeVerdict, Map<String, Value>) {
    let Value::Object(mut fields) = params else {
        return (BridgeVerdict::malformed(), Map::new());
    };
    let verdict = fields
        .remove(BRIDGE_FIELD)
        .and_then(|raw| serde_json::from_value::<BridgeAssertion>(raw).ok())
        .filter(BridgeAssertion::is_consistent)
        .map_or_else(BridgeVerdict::malformed, BridgeVerdict::asserted);
    (verdict, fields)
}

/// Whether the socket peer is the verified console.
///
/// Why: the server takes `agent_parent` only from the console (Architect
/// 04:03Z); any other peer could clear its own verdict.
/// What: the peer pid the router read must exist, its executable (from the
/// process table) must be named [`CONSOLE_EXECUTABLE`], and its own
/// ancestry must be readable and hold no Claude Code process. Any failure is
/// [`ErrorKind::BridgeRefused`].
/// Test: `bridge_assertion_from_non_console_peer_is_refused`,
/// `bridge_console_with_unreadable_chain_is_refused`,
/// `bridge_with_no_peer_pid_is_refused`.
fn verify_console(state: &State, caller: Caller) -> Result<(), ErrorKind> {
    let pid = caller.pid().ok_or(ErrorKind::BridgeRefused)?;
    let table: &dyn ProcessTable = state.grants.processes();
    let exe = table
        .executable(pid)
        .map_err(|_| ErrorKind::BridgeRefused)?;
    if exe.file_name() != Some(OsStr::new(CONSOLE_EXECUTABLE)) {
        return Err(ErrorKind::BridgeRefused);
    }
    match has_agent_ancestor(table, pid) {
        Ok(false) => Ok(()),
        Ok(true) | Err(_) => Err(ErrorKind::BridgeRefused),
    }
}
