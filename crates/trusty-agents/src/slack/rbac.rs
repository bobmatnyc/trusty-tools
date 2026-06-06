//! Per-user RBAC configuration for the Slack gateway (#480/#481).
//!
//! Why: The Slack bot is shared across the Duetto engineering team, but the
//! underlying `cto-assistant` persona reaches sensitive HR/budget data. Each
//! known user is pinned to a `ServiceTier` (gating the persona toolset) and an
//! optional persona allow-list (gating `/slack-switch`); unknown users get a
//! static Virtual-CTO reply that never touches the LLM.
//! What: `SlackUserConfig`, `SlackRbacConfig`, the env/string parsers, the
//! default team table, and the `UserIdentity` translation point.
//! Test: `rbac_config_parses_env_string`, `rbac_unknown_user_returns_virtual_cto_message`,
//! `switch_command_blocked_for_restricted_persona` in `slack::tests`.

use std::collections::HashMap;

use tracing::warn;

/// Per-user RBAC config for Slack (#481).
///
/// Why: The Slack bot is shared across the Duetto engineering team, but the
/// underlying `cto-assistant` persona reaches sensitive HR/budget data. Each
/// known user is pinned to a `ServiceTier` (which gates the persona toolset
/// via `filter_tools_for_user`) and an optional persona allow-list (which
/// gates `/slack-switch`). Unknown users fall through to a Virtual-CTO reply.
/// What: A flat record keyed by Slack user id.
/// Test: `rbac_config_parses_env_string`, `switch_command_blocked_for_restricted_persona`.
#[derive(Debug, Clone)]
pub struct SlackUserConfig {
    pub slack_id: String,
    pub name: String,
    pub tier: crate::rbac::ServiceTier,
    /// Allowed persona names. `None` means unrestricted (any persona).
    pub allowed_personas: Option<Vec<String>>,
}

/// Bot-wide Slack RBAC configuration (#480/#481).
///
/// Why: Centralizes the env-driven user table and default persona so
/// `run_slack_bot` and its handlers take a single `Arc<SlackRbacConfig>`
/// rather than re-parsing env on every message.
/// What: A user table keyed by Slack id plus the default persona name.
/// Test: `rbac_config_parses_env_string`, `rbac_unknown_user_returns_virtual_cto_message`.
#[derive(Debug, Clone)]
pub struct SlackRbacConfig {
    /// Keyed by Slack user ID.
    pub(super) users: HashMap<String, SlackUserConfig>,
    /// Default persona for all messages (from `SLACK_DEFAULT_PERSONA`,
    /// default `"cto-assistant"`).
    pub default_persona: String,
}

/// Static reply for Slack users not in the RBAC table (#481).
///
/// Why: Unknown users must NOT reach the LLM or any tool — the bot speaks as
/// a general "Virtual CTO" with no internal-data access. Returned verbatim,
/// bypassing `run_pm_task_with_persona` entirely.
pub(super) const VIRTUAL_CTO_MESSAGE: &str = ":lock: This assistant is for Duetto engineering team members. \
I can discuss general technology strategy and software architecture, but I don't have access to \
internal Duetto data. Feel free to ask general questions.";

impl SlackRbacConfig {
    /// Parse the RBAC config from process env.
    ///
    /// Why: Lets ops configure the user table without a code change. Falls
    /// back to a sensible hardcoded team list when `SLACK_RBAC_USERS` is
    /// absent so the bot is usable out of the box.
    /// What: `SLACK_DEFAULT_PERSONA` → `default_persona` (default
    /// `"cto-assistant"`). `SLACK_RBAC_USERS` → comma-separated
    /// `ID:Name:TIER:PERSONAS` entries; `PERSONAS` is `*` (unrestricted) or a
    /// `+`-separated allow-list.
    /// Test: `rbac_config_parses_env_string`.
    pub fn from_env() -> Self {
        let default_persona = std::env::var("SLACK_DEFAULT_PERSONA")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "cto-assistant".to_string());
        let raw = std::env::var("SLACK_RBAC_USERS").ok();
        let users = match raw.as_deref() {
            Some(s) if !s.trim().is_empty() => parse_rbac_users(s),
            _ => default_rbac_users(),
        };
        Self {
            users,
            default_persona,
        }
    }

    /// Look up a Slack user id in the RBAC table.
    pub fn user(&self, slack_id: &str) -> Option<&SlackUserConfig> {
        self.users.get(slack_id)
    }
}

/// Parse a `SLACK_RBAC_USERS` env string into a user table.
///
/// Why: Pure function so it can be unit-tested without touching process env.
/// What: Splits on `,` for entries and `:` for the 4 fields. Malformed
/// entries (wrong field count, unknown tier) are skipped with a warning.
/// Test: `rbac_config_parses_env_string`.
pub(super) fn parse_rbac_users(raw: &str) -> HashMap<String, SlackUserConfig> {
    let mut map = HashMap::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.split(':').collect();
        if parts.len() != 4 {
            warn!(entry, "slack rbac: skipping malformed user entry");
            continue;
        }
        let tier = match parts[2].trim().to_ascii_uppercase().as_str() {
            "ALL" => crate::rbac::ServiceTier::All,
            "ANALYTICS" => crate::rbac::ServiceTier::Analytics,
            "READONLY" => crate::rbac::ServiceTier::ReadOnly,
            other => {
                warn!(tier = other, entry, "slack rbac: unknown tier; skipping");
                continue;
            }
        };
        let allowed_personas = if parts[3].trim() == "*" {
            None
        } else {
            Some(
                parts[3]
                    .split('+')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        };
        let slack_id = parts[0].trim().to_string();
        map.insert(
            slack_id.clone(),
            SlackUserConfig {
                slack_id,
                name: parts[1].trim().to_string(),
                tier,
                allowed_personas,
            },
        );
    }
    map
}

/// Hardcoded default team RBAC table used when `SLACK_RBAC_USERS` is unset.
///
/// Why: The bot should be usable without ops first setting an env var.
/// Test: `rbac_config_parses_env_string` (indirectly via from_env fallback).
pub(super) fn default_rbac_users() -> HashMap<String, SlackUserConfig> {
    parse_rbac_users(
        "U0A6V2W1M2R:Masa:ALL:*,\
         U0ALDQLBU79:Andrea:ALL:cto-assistant,\
         U09331EP3MX:Alex:ANALYTICS:cto-assistant",
    )
}

/// Build a ctrl `UserIdentity` from a Slack RBAC user entry (#481).
///
/// Why: `run_pm_task_with_persona` gates the persona toolset by
/// `UserIdentity.tier`; this is the single translation point from the
/// Slack-native `SlackUserConfig` to the transport-agnostic identity.
/// Test: exercised via `rbac_unknown_user_returns_virtual_cto_message` and
/// `switch_command_blocked_for_restricted_persona`.
pub(super) fn identity_from_slack_user(u: &SlackUserConfig) -> crate::rbac::UserIdentity {
    crate::rbac::UserIdentity::new(u.slack_id.clone(), u.name.clone(), u.tier.clone())
}
