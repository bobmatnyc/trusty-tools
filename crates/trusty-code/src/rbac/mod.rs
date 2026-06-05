//! Role-based access control for tool execution in tcode.
//!
//! Why: tcode exposes tools over multiple transports (CLI, API, TUI). Some
//! tools (memory writes, shell exec) are unsafe for arbitrary callers while
//! still being essential for the trusted operator. RBAC gates tool execution
//! on a small, totally-ordered tier ladder so the same agent definition can be
//! safely exposed across transports without per-deployment code branches.
//! What: Defines `ServiceTier` (re-exported from `tools::traits`),
//! `UserIdentity` (the per-request principal), and `filter_tools_for_user`.
//! Test: See unit tests below — covers default identity, every tier × restriction
//! combination, and the filter helper.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use crate::tools::traits::ServiceTier;
use crate::tools::traits::ToolExecutor;

/// Per-request principal carried from transport into tool dispatch.
///
/// Why: Every inbound request (CLI, API call, TUI keystroke) arrives with some
/// notion of "who is asking". Modeling it as a single `UserIdentity` value
/// means the tool dispatch layer doesn't have to care which transport produced
/// the request, and unit tests can construct one without spinning up a transport.
/// What: `id` is the transport-native identifier; `name` is a human-readable
/// label for logging; `tier` is the privilege bucket enforced by `can_access_tier`.
/// Test: `default_is_local_cli_all`, plus indirect coverage via `can_access_tier` cases.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserIdentity {
    /// Transport-native identifier (e.g. `"cli"`, `"api"`, Slack `"U…"`).
    pub id: String,
    /// Human-readable label, used only for logging.
    pub name: String,
    /// Privilege tier enforced at dispatch.
    pub tier: ServiceTier,
}

impl Default for UserIdentity {
    fn default() -> Self {
        Self {
            id: "cli".into(),
            name: "local".into(),
            tier: ServiceTier::All,
        }
    }
}

impl UserIdentity {
    /// Construct a new identity with a given tier.
    ///
    /// Why: Transport layers need to mint identities with custom IDs per
    /// inbound request without hand-rolling the struct literal.
    /// What: Helper constructor; no validation beyond moving fields.
    /// Test: `new_constructs_with_fields`.
    pub fn new(id: impl Into<String>, name: impl Into<String>, tier: ServiceTier) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            tier,
        }
    }

    /// Whether this identity may invoke a tool whose `restricted_tiers` list
    /// is `restricted`.
    ///
    /// Why: Tools express restrictions as an *inverted* list — they name the
    /// tiers that are NOT allowed to call them — because that reads naturally
    /// in config files (`restricted_tiers = ["read_only"]` blocks one tier).
    /// An empty restriction list always permits access.
    /// What: Returns `true` if `restricted` is empty, or if `self.tier` is not
    /// present in `restricted`. Check is membership-based, NOT ordering-based.
    /// Test: `can_access_tier_empty_always_allows`, `can_access_tier_blocks_listed_tier`,
    /// `can_access_tier_allows_other_tiers`.
    pub fn can_access_tier(&self, restricted: &[ServiceTier]) -> bool {
        restricted.is_empty() || !restricted.contains(&self.tier)
    }
}

/// Filter a list of tools to those callable by `user`.
///
/// Why: The LLM should not even see tools it isn't allowed to call — presenting
/// them and then denying at dispatch wastes context and invites retries.
/// What: Drops any tool whose `restricted_tiers()` blocks `user.tier`. Tools
/// that haven't opted into RBAC (empty `restricted_tiers`) pass through unchanged.
/// Test: Exercised in `ToolRegistry::filter_tools_for_user` tests.
pub fn filter_tools_for_user(
    tools: &[Arc<dyn ToolExecutor>],
    user: &UserIdentity,
) -> Vec<Arc<dyn ToolExecutor>> {
    tools
        .iter()
        .filter(|t| user.can_access_tier(t.restricted_tiers()))
        .cloned()
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_service_tier_is_all() {
        assert_eq!(ServiceTier::default(), ServiceTier::All);
    }

    #[test]
    fn service_tier_serializes_snake_case() {
        let s = serde_json::to_string(&ServiceTier::ReadOnly).expect("serialize");
        assert_eq!(s, "\"read_only\"");
        let r: ServiceTier = serde_json::from_str("\"analytics\"").expect("deserialize");
        assert_eq!(r, ServiceTier::Analytics);
    }

    #[test]
    fn default_identity_is_local_cli_all() {
        let u = UserIdentity::default();
        assert_eq!(u.id, "cli");
        assert_eq!(u.name, "local");
        assert_eq!(u.tier, ServiceTier::All);
    }

    #[test]
    fn new_constructs_with_fields() {
        let u = UserIdentity::new("U123", "alice", ServiceTier::Analytics);
        assert_eq!(u.id, "U123");
        assert_eq!(u.name, "alice");
        assert_eq!(u.tier, ServiceTier::Analytics);
    }

    #[test]
    fn can_access_tier_empty_always_allows() {
        let u = UserIdentity::new("x", "x", ServiceTier::ReadOnly);
        assert!(u.can_access_tier(&[]));
        let u = UserIdentity::new("x", "x", ServiceTier::All);
        assert!(u.can_access_tier(&[]));
    }

    #[test]
    fn can_access_tier_blocks_listed_tier() {
        let u = UserIdentity::new("x", "x", ServiceTier::ReadOnly);
        assert!(!u.can_access_tier(&[ServiceTier::ReadOnly]));
        assert!(!u.can_access_tier(&[ServiceTier::All, ServiceTier::ReadOnly]));
    }

    #[test]
    fn can_access_tier_allows_other_tiers() {
        // `All` is not in the blocklist — allowed even though `ReadOnly` is named.
        let u = UserIdentity::new("x", "x", ServiceTier::All);
        assert!(u.can_access_tier(&[ServiceTier::ReadOnly]));
        let u = UserIdentity::new("x", "x", ServiceTier::Analytics);
        assert!(u.can_access_tier(&[ServiceTier::ReadOnly]));
        assert!(!u.can_access_tier(&[ServiceTier::Analytics]));
    }

    #[test]
    fn can_access_tier_no_implicit_ordering() {
        // Restrictions are explicit memberships, not ordered comparisons.
        let u = UserIdentity::new("x", "x", ServiceTier::ReadOnly);
        assert!(u.can_access_tier(&[ServiceTier::Analytics]));
    }
}
