//! Role-based access control for tool execution (#445).
//!
//! Why: Open-mpm exposes tools over multiple transports (CLI, REPL, Slack,
//! Telegram, HTTP API). Some tools (memory writes, shell exec, ticketing
//! mutations) are unsafe for arbitrary end-users while still being essential
//! for the trusted operator at the controller. RBAC gates tool execution on
//! a small, totally-ordered tier ladder so the same agent definition can be
//! safely exposed across transports without per-deployment code branches.
//! What: Defines `ServiceTier` (the access-level enum), `UserIdentity` (the
//! per-request principal carried from transport into dispatch), and
//! `can_access_tier` / `filter_tools_for_user` helpers that tool dispatch
//! and schema-emission sites consume.
//! Test: See `tests` submodule — covers default identity, every tier × every
//! restriction combination, and the filter helper for the empty / one-tier /
//! mixed cases. Higher-level dispatch enforcement is covered in
//! `tools/mod.rs::dispatch_for_user_*` tests.

use serde::{Deserialize, Serialize};

/// Access tier assigned to a `UserIdentity`.
///
/// Why: Re-exported from `trusty-agents-common` (which had to own the type so
///      external agent crates can implement `ToolExecutor::restricted_tiers`
///      without depending on this crate). The original definition lived here;
///      consolidating into the agent-api crate broke the cargo cycle between
///      `trusty-agents` and `cto-assistant`. Internal call sites still write
///      `crate::rbac::ServiceTier` and get the same type.
/// What: Re-export of `trusty_agents_common::ServiceTier`. Variants, serde
///       conventions (`snake_case`), and `Default = All` are unchanged.
/// Test: `service_tier_serializes_snake_case`, `default_is_all` still pass
///       because the type is the same one.
pub use trusty_agents_common::ServiceTier;

/// Per-request principal carried from transport into tool dispatch.
///
/// Why: Every inbound request (Slack message, Telegram update, HTTP request,
/// CLI/REPL keystroke) arrives with some notion of "who is asking". Modeling
/// it as a single `UserIdentity` value means the tool dispatch layer doesn't
/// have to care which transport produced the request, and unit tests can
/// construct one without spinning up a transport.
/// What: `id` is the transport-native identifier (Slack `U…`, Telegram numeric
/// id, `"cli"`, `"api"`). `name` is a human-readable label used only for
/// logging. `tier` is the privilege bucket enforced by `can_access_tier`.
/// Test: `default_is_local_cli_all`, plus indirect coverage via
/// `can_access_tier` cases.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserIdentity {
    pub id: String,
    pub name: String,
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
    /// Why: Transport layers (slack/telegram/api) need to mint identities
    /// with a custom `id` per inbound request without hand-rolling the
    /// struct literal.
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
    /// tiers that are NOT allowed to call them — because that reads more
    /// naturally in TOML (`restricted_tiers = ["read_only"]` blocks one tier
    /// rather than enumerating allowed ones). An empty restriction list
    /// always permits access, so adding RBAC to existing tools is a no-op.
    /// What: Returns `true` if `restricted` is empty, or if `self.tier` is
    /// not present in `restricted`. The check is membership-based and does
    /// NOT use tier ordering — restrictions are explicit, not implied by
    /// hierarchy.
    /// Test: `can_access_tier_empty_always_allows`,
    /// `can_access_tier_blocks_listed_tier`, `can_access_tier_allows_other_tiers`.
    pub fn can_access_tier(&self, restricted: &[ServiceTier]) -> bool {
        restricted.is_empty() || !restricted.contains(&self.tier)
    }
}

/// Filter a list of tools to those callable by `user`.
///
/// Why: The LLM should not even see tools it isn't allowed to call —
/// presenting them and then denying at dispatch wastes context and invites
/// the model to retry. Filtering at schema-emission time keeps the LLM's
/// world view consistent with what dispatch will actually permit.
/// What: Drops any tool whose `restricted_tiers()` blocks `user.tier`. The
/// `ToolExecutor` trait's `restricted_tiers()` defaults to an empty slice,
/// so tools that haven't opted into RBAC pass through unchanged. Returns
/// a cloned `Vec<Arc<dyn ToolExecutor>>` so the caller can build a filtered
/// schema list without taking ownership of the original registry.
/// Test: `tools/mod.rs::filter_tools_for_user_*`.
pub fn filter_tools_for_user(
    tools: &[std::sync::Arc<dyn crate::tools::ToolExecutor>],
    user: &UserIdentity,
) -> Vec<std::sync::Arc<dyn crate::tools::ToolExecutor>> {
    tools
        .iter()
        .filter(|t| user.can_access_tier(t.restricted_tiers()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all() {
        assert_eq!(ServiceTier::default(), ServiceTier::All);
    }

    #[test]
    fn service_tier_serializes_snake_case() {
        let s = serde_json::to_string(&ServiceTier::ReadOnly).unwrap();
        assert_eq!(s, "\"read_only\"");
        let r: ServiceTier = serde_json::from_str("\"analytics\"").unwrap();
        assert_eq!(r, ServiceTier::Analytics);
    }

    #[test]
    fn default_is_local_cli_all() {
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
        let u = UserIdentity::new("x", "x", ServiceTier::All);
        // `All` is not in the blocklist — request allowed even though
        // ReadOnly is named (no implicit hierarchy).
        assert!(u.can_access_tier(&[ServiceTier::ReadOnly]));
        let u = UserIdentity::new("x", "x", ServiceTier::Analytics);
        assert!(u.can_access_tier(&[ServiceTier::ReadOnly]));
        assert!(!u.can_access_tier(&[ServiceTier::Analytics]));
    }

    #[test]
    fn can_access_tier_no_implicit_ordering() {
        // ReadOnly is strict but if a tool only blocks Analytics, ReadOnly
        // is still allowed — restrictions are explicit memberships, not
        // ordered comparisons.
        let u = UserIdentity::new("x", "x", ServiceTier::ReadOnly);
        assert!(u.can_access_tier(&[ServiceTier::Analytics]));
    }
}
