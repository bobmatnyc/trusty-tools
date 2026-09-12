//! `[permissions]` — the structured permissions model (#3936, DOC-57 §7).
//!
//! Why: An agent's permission posture is spread across four unrelated
//! mechanisms in three locations with three different failure polarities
//! (DOC-57 §7.1): `[tools].allow` globs (absent ⇒ zero tools), `[tools].scopes`
//! (empty ⇒ denies everything), RBAC tiers (default ⇒ degrades open), and the
//! harness-level endpoint scope filter. `[permissions]` **describes** that
//! union — it does not invent new enforcement (PM-1) — so a GUI pane can show
//! one coherent model instead of four scattered mechanisms.
//! What: [`PermissionsConfig`] is the `[permissions]` section of `agent.toml`.
//! [`effective_scopes`] is the ONE place CC-9 precedence
//! (`[permissions].scopes` wins over legacy `[tools].scopes` when a single
//! file declares both — the union is deliberately NOT taken, DOC-57 §9.4) is
//! decided, so the persona-chat dispatch gate (`ctrl::pm_task::dispatch::persona`)
//! and the `GET /api/agents/:name/permissions` route can never disagree about
//! what is actually enforced. [`dropped_scopes`] is that decision's readable
//! half (#4158) — the legacy entries CC-9 removed, so the route can report a
//! drop the `tracing::warn!` alone never reaches an operator with.
//! Test: `effective_scopes_prefers_permissions_over_legacy_tools_scopes`,
//! `effective_scopes_falls_back_to_tools_scopes_when_permissions_absent`,
//! `effective_scopes_absent_both_is_none`.

use serde::{Deserialize, Serialize};

use super::config::ToolsConfig;
use crate::rbac::ServiceTier;

/// `[permissions]` section (#3936, DOC-57 §7.2).
///
/// Why: Promotes scopes/tiers/authority/autonomy from four scattered
/// mechanisms into one declared, first-class section. Every field either maps
/// to an existing enforced mechanism or is explicitly reserved/declarative
/// (PM-1) — the route that reports this config (`agent_permissions.rs`) is
/// what makes that distinction visible via its `enforced` flags.
/// What: `scopes` supersedes legacy `[tools].scopes` per-file (CC-9,
/// [`effective_scopes`]). `user_authority` is DOC-41 §5.5's reserved
/// singleton — parsed and surfaced now, enforced when #3074 lands.
/// `default_tier`/`unauthenticated_tier` supersede legacy `[rbac]` fields.
/// `autonomy` is a declarative enum (DOC-23 owns the real decision model,
/// PM-2). `grants` is a declarative per-skill override list — Phase 4 ships
/// it read-only; there is no enforcement site for it yet (PM-1, PM-4).
/// Test: `permissions_config_parses_full_block`,
/// `permissions_config_defaults_when_absent`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct PermissionsConfig {
    /// Settings list replacements supersede inherited grants; authored overlays default to union.
    /// Test: `settings_grant_replacements_survive_extends`.
    #[serde(default)]
    pub replace_scopes: bool,
    #[serde(default)]
    pub replace_tools: bool,
    #[serde(default)]
    pub replace_skills: bool,
    #[serde(default)]
    pub replace_subagents: bool,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// DOC-41 §5.5's `user_authority` singleton. Reserved: parsed and
    /// surfaced, but nothing in this crate enforces it yet (#3074/#3075).
    /// Defaults `false`, matching the singleton's documented default.
    #[serde(default)]
    pub user_authority: bool,
    #[serde(default)]
    pub default_tier: Option<ServiceTier>,
    #[serde(default)]
    pub unauthenticated_tier: Option<ServiceTier>,
    #[serde(default)]
    pub autonomy: Option<AutonomyMode>,
    #[serde(default)]
    pub grants: Vec<PermissionGrant>,
}

/// DOC-54 §2.1's ask-first / learn-to-act posture (§7.2). Declarative only in
/// M1 — the real decision model is DOC-23 (PM-2, OQ-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutonomyMode {
    #[default]
    AskFirst,
    LearnToAct,
}

/// One `[[permissions.grants]]` entry — a per-skill override (§7.2).
///
/// Why: Declarative in M1 (PM-1/PM-4) — there is no enforcement site for a
/// per-skill grant mode yet, so this is data the route reports, not a control
/// the dispatch gate consults.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PermissionGrant {
    pub skill: String,
    pub mode: GrantMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantMode {
    Allow,
    Ask,
    Deny,
}

/// Resolve the effective declared scope patterns for ONE agent file (CC-9).
///
/// Why: **This is the whole compatibility + honesty story for scopes.**
/// `[permissions].scopes` supersedes legacy `[tools].scopes` when BOTH are
/// declared in the same file — the union is deliberately NOT taken (DOC-57
/// §9.4: unioning a legacy and a new declaration could only ever WIDEN the
/// scope set, which is the wrong default for a migration path). Calling this
/// function from both the persona-chat dispatch gate
/// (`ctrl::pm_task::dispatch::persona`) and the `/permissions` route is what
/// lets the route's `enforced: true` claim for a `[permissions].scopes]`-only
/// agent be TRUE rather than aspirational — see PM-6.
/// What: `Some(permissions.scopes)` when declared (logging a WARN when
/// `tools.scopes` is ALSO declared and differs, naming both so the file is
/// self-diagnosing); otherwise `tools.scopes.clone()`; `None` when neither is
/// declared (preserves today's absent-denies-all polarity exactly).
/// Test: `effective_scopes_prefers_permissions_over_legacy_tools_scopes`,
/// `effective_scopes_falls_back_to_tools_scopes_when_permissions_absent`,
/// `effective_scopes_absent_both_is_none`,
/// `effective_scopes_is_byte_identical_when_permissions_matches_tools`.
pub fn effective_scopes(
    tools: &ToolsConfig,
    permissions: &PermissionsConfig,
) -> Option<Vec<String>> {
    if let Some(scopes) = &permissions.scopes {
        if let Some(legacy) = &tools.scopes
            && legacy != scopes
        {
            tracing::warn!(
                permissions_scopes = ?scopes,
                tools_scopes = ?legacy,
                "agent declares both [permissions].scopes and legacy [tools].scopes with \
                 different values; [permissions].scopes wins per DOC-57 CC-9 — the union is \
                 not taken"
            );
        }
        return Some(scopes.clone());
    }
    tools.scopes.clone()
}

/// Why one legacy `[tools].scopes` entry is missing from the effective set.
///
/// Why: A bare list of dropped patterns tells an operator that something
/// vanished, not what removed it. Naming the rule — and that CC-9 takes a
/// winner rather than a union — is what makes the `/permissions` pane's
/// `dropped_scopes[]` (#4158) actionable instead of merely alarming.
/// What: the ONE reason a scope can be dropped today. A second drop rule
/// would add a second constant beside this one rather than generalise its
/// wording.
/// Test: `dropped_scopes_lists_only_the_superseded_legacy_entries`.
pub const SCOPE_DROPPED_SUPERSEDED: &str = concat!(
    "declared in legacy [tools].scopes but absent from [permissions].scopes; ",
    "DOC-57 CC-9 takes the winning declaration, not the union",
);

/// Legacy `[tools].scopes` entries that [`effective_scopes`] leaves out (#4158).
///
/// Why: CC-9's silent-drop is the exact silent-capability-loss class that has
/// repeatedly cost investigation time here (#3844, #3987, #4093). Until this
/// function existed the only signal was [`effective_scopes`]'s `tracing::warn!`,
/// which no API or GUI consumer reads — and the app-launched daemon sends its
/// logs to `/dev/null` (#4111), so in the shipped product that warning does not
/// exist at all. This is the readable half of that same signal.
/// What: the entries of `tools.scopes` that are not in `permissions.scopes`,
/// in declaration order, when BOTH are declared; empty otherwise (nothing was
/// superseded, so nothing was dropped). Deliberately NOT the same condition as
/// the warning, which fires on any difference: a `[permissions].scopes` that
/// only ADDS to the legacy list drops nothing and must report nothing.
/// Pair each entry with [`SCOPE_DROPPED_SUPERSEDED`] when reporting it.
/// Test: `dropped_scopes_lists_only_the_superseded_legacy_entries`,
/// `dropped_scopes_is_empty_when_permissions_only_widens`,
/// `dropped_scopes_is_empty_without_a_permissions_declaration`.
pub fn dropped_scopes(tools: &ToolsConfig, permissions: &PermissionsConfig) -> Vec<String> {
    let (Some(winning), Some(legacy)) = (&permissions.scopes, &tools.scopes) else {
        return Vec::new();
    };
    legacy
        .iter()
        .filter(|pattern| !winning.contains(pattern))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools_with_scopes(scopes: Option<Vec<&str>>) -> ToolsConfig {
        ToolsConfig {
            scopes: scopes.map(|v| v.into_iter().map(str::to_string).collect()),
            ..Default::default()
        }
    }

    fn permissions_with_scopes(scopes: Option<Vec<&str>>) -> PermissionsConfig {
        PermissionsConfig {
            scopes: scopes.map(|v| v.into_iter().map(str::to_string).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn effective_scopes_prefers_permissions_over_legacy_tools_scopes() {
        let tools = tools_with_scopes(Some(vec!["memory.read"]));
        let permissions = permissions_with_scopes(Some(vec!["google.gmail.*"]));
        assert_eq!(
            effective_scopes(&tools, &permissions),
            Some(vec!["google.gmail.*".to_string()])
        );
    }

    #[test]
    fn effective_scopes_falls_back_to_tools_scopes_when_permissions_absent() {
        let tools = tools_with_scopes(Some(vec!["memory.read"]));
        let permissions = permissions_with_scopes(None);
        assert_eq!(
            effective_scopes(&tools, &permissions),
            Some(vec!["memory.read".to_string()])
        );
    }

    #[test]
    fn effective_scopes_absent_both_is_none() {
        let tools = tools_with_scopes(None);
        let permissions = permissions_with_scopes(None);
        assert_eq!(effective_scopes(&tools, &permissions), None);
    }

    #[test]
    fn effective_scopes_is_byte_identical_when_permissions_matches_tools() {
        let tools = tools_with_scopes(Some(vec!["search.read"]));
        let permissions = permissions_with_scopes(Some(vec!["search.read"]));
        assert_eq!(
            effective_scopes(&tools, &permissions),
            Some(vec!["search.read".to_string()])
        );
    }

    // #4158: the drop list must name exactly what CC-9 removed — not every
    // legacy entry, and not one the new declaration restates.
    #[test]
    fn dropped_scopes_lists_only_the_superseded_legacy_entries() {
        let tools = tools_with_scopes(Some(vec!["memory.read", "search.read"]));
        let permissions = permissions_with_scopes(Some(vec!["search.read", "google.gmail.*"]));
        assert_eq!(
            dropped_scopes(&tools, &permissions),
            vec!["memory.read".to_string()],
            "search.read survives because the new declaration restates it"
        );
        assert!(!SCOPE_DROPPED_SUPERSEDED.is_empty());
    }

    #[test]
    fn dropped_scopes_is_empty_when_permissions_only_widens() {
        // `effective_scopes` WARNS here (the lists differ) but nothing is
        // lost, so the operator-facing list must stay empty.
        let tools = tools_with_scopes(Some(vec!["memory.read"]));
        let permissions = permissions_with_scopes(Some(vec!["memory.read", "search.read"]));
        assert!(dropped_scopes(&tools, &permissions).is_empty());
    }

    #[test]
    fn dropped_scopes_is_empty_without_a_permissions_declaration() {
        let tools = tools_with_scopes(Some(vec!["memory.read"]));
        assert!(dropped_scopes(&tools, &permissions_with_scopes(None)).is_empty());
        assert!(
            dropped_scopes(&tools_with_scopes(None), &permissions_with_scopes(None)).is_empty()
        );
    }

    #[test]
    fn permissions_config_parses_full_block() {
        let raw = r#"
[agent]
name = "x"

[permissions]
scopes = ["memory.read", "google.gmail.*"]
user_authority = true
default_tier = "analytics"
unauthenticated_tier = "read_only"
autonomy = "learn-to-act"

[[permissions.grants]]
skill = "gmail"
mode = "ask"
"#;
        #[derive(Deserialize)]
        struct W {
            permissions: PermissionsConfig,
        }
        let w: W = toml::from_str(raw).unwrap();
        assert_eq!(
            w.permissions.scopes,
            Some(vec![
                "memory.read".to_string(),
                "google.gmail.*".to_string()
            ])
        );
        assert!(w.permissions.user_authority);
        assert_eq!(w.permissions.default_tier, Some(ServiceTier::Analytics));
        assert_eq!(
            w.permissions.unauthenticated_tier,
            Some(ServiceTier::ReadOnly)
        );
        assert_eq!(w.permissions.autonomy, Some(AutonomyMode::LearnToAct));
        assert_eq!(w.permissions.grants.len(), 1);
        assert_eq!(w.permissions.grants[0].skill, "gmail");
        assert_eq!(w.permissions.grants[0].mode, GrantMode::Ask);
    }

    #[test]
    fn permissions_config_defaults_when_absent() {
        #[derive(Deserialize, Default)]
        struct W {
            #[serde(default)]
            permissions: PermissionsConfig,
        }
        let w: W = toml::from_str("[agent]\nname = \"x\"\n").unwrap();
        assert_eq!(w.permissions, PermissionsConfig::default());
        assert!(!w.permissions.user_authority);
        assert_eq!(w.permissions.autonomy, None);
    }
}
