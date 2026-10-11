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

use super::methods::method as direct;

/// The params field that carries the console's [`BridgeAssertion`].
pub const BRIDGE_FIELD: &str = "bridge";

/// The bridged method names. Each runs its direct method under the console's
/// verdict. `scopes` and `doctor` read no credential and are not bridged;
/// `grant`, `resolve` and `revoke` have no console route.
pub mod method {
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

/// The direct method a bridged method runs; `None` for any other name.
///
/// What: the server answers a bridged call's errors under this name and
/// gives it this method's deadline, so the console sees what a direct
/// caller sees.
pub fn direct_method(bridged: &str) -> Option<&'static str> {
    match bridged {
        method::LIST => Some(direct::LIST),
        method::SET => Some(direct::SET),
        method::DELETE => Some(direct::DELETE),
        method::COPY => Some(direct::COPY),
        method::SET_AGENTS_MAY_USE => Some(direct::SET_AGENTS_MAY_USE),
        _ => None,
    }
}

/// How the console judged its HTTP client.
///
/// What: `mapped` — the client's pid was found and its chain read;
/// `unmappable` — no pid could be found (a non-agent, Architect ruling
/// 2026-10-11); `unreadable` — a pid was found but its chain could not be
/// read, which the server treats as the direct socket treats an unreadable
/// chain.
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
/// `unmappable` must carry `false` and `unreadable` must carry `true`, so
/// the boolean alone never under-claims; the server refuses any other
/// pairing, and any unknown field, as `invalid_params`. Build one with
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

    /// A client with no pid: a non-agent.
    pub fn unmappable() -> Self {
        Self {
            agent_parent: false,
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
            BridgeSource::Unmappable => !self.agent_parent,
            BridgeSource::Unreadable => self.agent_parent,
        }
    }
}
