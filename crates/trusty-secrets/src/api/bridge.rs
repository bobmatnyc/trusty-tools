//! The console bridge's assertion and method names (DOC-74 §15.6, #9066).
//!
//! Why: Dd7 option B (owner, 2026-10-11) — a call the console bridge
//! forwards gets the verdict a direct socket call gets. Only the console can
//! map the HTTP client to a pid; the socket sees the console. So it judges that
//! client's ancestry and asserts the result; the server takes the assertion
//! only from the verified console process. The console links `api` only, so
//! the assertion type lives here.
//! What: [`BRIDGE_FIELD`], the params field that carries a
//! [`BridgeAssertion`] beside a method's own params; [`method`], the bridged
//! method names; and [`direct_method`], the direct method each one runs.
//! Test: `api_bridge_assertion_wire_shape_and_consistency`.

use serde::{Deserialize, Serialize};

use super::BackendId;
use super::methods::method as direct;

/// The params field that carries the console's [`BridgeAssertion`].
pub const BRIDGE_FIELD: &str = "bridge";

/// The bridged method names. Each runs its direct method under the console's
/// verdict, except `backends`, which has no direct form. `doctor` is not
/// bridged; `grant`, `resolve` and `revoke` have no console route.
pub mod method {
    /// The backends the page's copy dialog may offer: `secrets.doctor`'s
    /// judgement for one project, reduced to id and availability (#9067 P1).
    /// Bridge-only; it has no direct method.
    pub const BACKENDS: &str = "secrets.bridge.backends";
    /// `secrets.scopes` through the console bridge: the page's scope picker
    /// (#9067).
    pub const SCOPES: &str = "secrets.bridge.scopes";
    /// `secrets.list` through the console bridge.
    pub const LIST: &str = "secrets.bridge.list";
    /// `secrets.set` through the console bridge.
    pub const SET: &str = "secrets.bridge.set";
    /// `secrets.delete` through the console bridge.
    pub const DELETE: &str = "secrets.bridge.delete";
    /// `secrets.copy` through the console bridge.
    pub const COPY: &str = "secrets.bridge.copy";
    /// `secrets.set_agents_may_use` through the console bridge.
    pub const SET_AGENTS_MAY_USE: &str = "secrets.bridge.set_agents_may_use";
}

/// The direct method a bridged method runs; `None` for any other name and
/// for [`method::BACKENDS`], which has no direct form (#9067) and answers
/// under its own name.
///
/// What: the server answers a bridged call's errors under this name and
/// gives it this method's deadline, so the console sees what a direct
/// caller sees.
pub fn direct_method(bridged: &str) -> Option<&'static str> {
    match bridged {
        method::SCOPES => Some(direct::SCOPES),
        method::LIST => Some(direct::LIST),
        method::SET => Some(direct::SET),
        method::DELETE => Some(direct::DELETE),
        method::COPY => Some(direct::COPY),
        method::SET_AGENTS_MAY_USE => Some(direct::SET_AGENTS_MAY_USE),
        _ => None,
    }
}

/// One backend in a [`BackendsResponse`].
///
/// What: the id and whether the server opens it now; never a path, reason
/// or detail. Unknown fields are refused, so a server that sent more would
/// not have it relayed to the page (#9067 P1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BackendChoice {
    /// The backend id.
    pub id: BackendId,
    /// Whether this server opens it now.
    pub available: bool,
}

/// `secrets.bridge.backends` response (#9067 P1).
///
/// Why: the copy dialog must offer the backends this host has, not a fixed
/// list: a Linux host has no Keychain.
/// What: the backend a write in the project uses, and every backend
/// `secrets.doctor` lists, in its order.
/// Test: `bridge_backends_match_doctor_and_are_audited_once`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BackendsResponse {
    /// The backend the project's writes use.
    pub selected: BackendId,
    /// Every backend, available or not.
    pub backends: Vec<BackendChoice>,
}

impl BackendsResponse {
    /// A response for `selected` over `backends`.
    pub fn new(selected: BackendId, backends: Vec<BackendChoice>) -> Self {
        Self { selected, backends }
    }
}

impl BackendChoice {
    /// One backend row.
    pub fn new(id: BackendId, available: bool) -> Self {
        Self { id, available }
    }
}

/// How the console judged its HTTP client.
///
/// What: `mapped` — the client's pid was found and its chain read;
/// `unmappable` — no pid could be found, so the client is treated as an
/// agent (#9067, Architect Q5: fail closed); `unreadable` — a pid was found
/// but its chain could not be read, which the server treats as the direct
/// socket treats an unreadable chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BridgeSource {
    /// The client's pid was found and its ancestry read.
    Mapped,
    /// No pid could be found for the client.
    Unmappable,
    /// The client's pid was found but its ancestry could not be read.
    Unreadable,
}

/// The console's verdict on the HTTP client it forwards a call for.
///
/// Why: the server never takes a pid as a parameter; the console sends only
/// a boolean and where it came from.
/// What: `agent_parent` is whether the client's chain holds Claude Code.
/// `unmappable` and `unreadable` must both carry `true`, so the boolean
/// alone never under-claims; the server refuses any other pairing, and any
/// unknown field, as `invalid_params`. Build one with
/// [`BridgeAssertion::mapped`], [`BridgeAssertion::unmappable`] or
/// [`BridgeAssertion::unreadable`].
/// Test: `api_bridge_assertion_wire_shape_and_consistency`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BridgeAssertion {
    /// Whether the client's ancestry holds a Claude Code process.
    pub agent_parent: bool,
    /// How the console reached that answer.
    pub source: BridgeSource,
}

impl BridgeAssertion {
    /// A client whose pid was found and whose chain was read.
    pub fn mapped(agent_parent: bool) -> Self {
        Self {
            agent_parent,
            source: BridgeSource::Mapped,
        }
    }

    /// A client with no pid: treated as an agent.
    pub fn unmappable() -> Self {
        Self {
            // #9067 Architect Q5: a client that cannot be mapped is never a person.
            agent_parent: true,
            source: BridgeSource::Unmappable,
        }
    }

    /// A client whose chain could not be read.
    pub fn unreadable() -> Self {
        Self {
            agent_parent: true,
            source: BridgeSource::Unreadable,
        }
    }

    /// Whether `agent_parent` agrees with `source`.
    pub fn is_consistent(&self) -> bool {
        match self.source {
            BridgeSource::Mapped => true,
            // #9067: unmappable fails closed, like unreadable.
            BridgeSource::Unmappable | BridgeSource::Unreadable => self.agent_parent,
        }
    }
}
