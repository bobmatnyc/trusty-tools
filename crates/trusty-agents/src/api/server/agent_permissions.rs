//! `GET /api/agents/:name/permissions` — the agent's structured permissions
//! model: scopes, tiers, `user_authority`, autonomy posture, per-skill
//! grants (#3936, DOC-57 §7).
//!
//! Why: An agent's permission posture is spread across four unrelated
//! mechanisms in three locations with three different failure polarities
//! (DOC-57 §7.1). This route is the ONE place a client reads them as a single
//! coherent model instead of four scattered files/comments.
//!
//! **Honesty rules this route holds to (PM-6):**
//! - Every element carries an `enforced` boolean. `scopes[]` is `enforced:
//!   true` — `agents::permissions::effective_scopes` (the SAME function
//!   `ctrl::pm_task::dispatch::persona` calls) feeds the SAME
//!   `agent_can_use`/`ScopePattern` gate — so this is not aspirational, BUT it
//!   is **not universal either**. `effective_scopes` is called from exactly
//!   ONE dispatch path in this crate: the persona-chat gate
//!   (`ctrl::pm_task::dispatch::persona`). The `--direct`/subprocess path
//!   (`runtime::subagent_mode` → `runtime::tool_registry::scope_assistant_allowed_tools`)
//!   filters purely by `[tools].allow` NAME globs and never reads
//!   `tools.scopes`, `permissions.scopes`, `ScopePattern`, or `agent_can_use`
//!   at all — an agent invoked via `tagent --direct` is NOT scope-restricted
//!   by anything this pane reports. Each scope element therefore ALSO carries
//!   `enforced_on: ["persona_chat"]` ([`SCOPE_ENFORCED_ON`]) so the JSON
//!   contract itself says so, not just this doc comment — a bare `enforced:
//!   true` with no qualifier is exactly the half-true "some path enforces
//!   this" reading that made #4018 a half-fix (#3987/#4093 track unifying the
//!   two paths; not attempted here). Every other field (`user_authority`,
//!   `tiers`, `autonomy`, `grants`) is `enforced: false` — none has a live
//!   enforcement site in this crate yet (tiers: DOC-57 §7.1 mechanism 3,
//!   "`[rbac]` block has no read site"; `user_authority`: #3074/#3075;
//!   `autonomy`: PM-2, DOC-23 owns the real model; `grants`: PM-1/PM-4, no
//!   enforcement site by design in this phase).
//! - `source` distinguishes `declared` (in this agent's OWN file) from
//!   `inherited:<base>` (only reachable via its immediate `extends` target),
//!   so the DOC-57 §7.3 class of defect (an agent granting tools whose
//!   scopes it never actually declared, `google.read` only) is visible in
//!   the pane rather than requiring a reader to simulate `merge_extends`.
//!   Multi-level provenance is approximated to the IMMEDIATE base — see
//!   [`permissions_at`]'s doc for why that is still a true statement.
//! - This route grants or widens nothing (C-06.4, PM-4) — it is a read
//!   surface over `AgentConfig::by_name_in`'s existing resolution.
//!
//! What: [`agent_permissions_route`] is the axum shim; [`permissions_at`] is
//! the testable core.
//! Test: `super::tests::agent_permissions`.

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::agents::permissions::{PermissionsConfig, effective_scopes};
use crate::agents::{AgentConfig, RbacConfig, ToolsConfig};
use crate::rbac::ServiceTier;

/// The dispatch path(s) `agents::permissions::effective_scopes` actually
/// gates, named on every `scopes[]` element (`enforced_on`).
///
/// Why: `enforced: true` alone reads as universal. It is not — the
/// `--direct`/subprocess path (`runtime::subagent_mode` →
/// `runtime::tool_registry::scope_assistant_allowed_tools`) filters by
/// `[tools].allow` name globs only and never consults a scope pattern at
/// all. Naming the ONE path that does (module doc) in the wire contract
/// itself, not only in a comment, is what keeps this pane from overstating
/// its own reach — the exact failure class (#4018) this crate has already
/// shipped once.
/// What: A single-element list today; grows if/when #3987/#4093 unify the
/// two dispatch paths' scope enforcement.
const SCOPE_ENFORCED_ON: &[&str] = &["persona_chat"];

/// `GET /api/agents/:name/permissions` — HTTP entry point.
///
/// Why/What: see the module doc.
/// Test: `super::tests::agent_permissions::permissions_route_reports_declared_and_inherited_scopes`.
pub(super) async fn agent_permissions_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    permissions_at(&crate::agents::agents_dir_candidates(), &name).await
}

/// Core permissions-resolution logic against an explicit agents-dir list.
///
/// Why: Same testability rationale as `agent_stores::stores_at` /
/// `agent_skills::skills_at`. Reads the agent's OWN file directly (partial
/// parse, matching the sibling routes' package-tolerant style) for the
/// `declared` half of `source`, AND resolves the full `extends` chain via
/// `AgentConfig::by_name_in` for the effective (chain-unioned) half — the
/// only route of the three that needs BOTH, because `source` (PM-7) is the
/// one field that requires knowing what this agent's OWN file said versus
/// what the chain as a whole resolves to.
/// What: `400` for an invalid name, `404` for an unknown agent, `500` only
/// when the resolved file cannot be read at all. Malformed TOML degrades to
/// `config_error` (never a `500`, K-1's posture applied here). An `extends`
/// chain that fails to resolve (missing base, cycle, depth) degrades to
/// this agent's own declarations only — every scope reports `source:
/// "declared"` rather than the route going dark.
///
/// Multi-level provenance approximation: `source` names the agent's
/// IMMEDIATE `extends` target for anything not in its own declared set, even
/// across a longer chain. This is still a TRUE statement — the pattern IS
/// reachable only via that immediate base, whatever THAT base in turn
/// inherited — and avoids re-deriving per-level provenance from
/// `merge_extends`'s flattened output, which does not retain it.
/// Test: `permissions_route_reports_declared_and_inherited_scopes`,
/// `permissions_route_never_inherits_user_authority`,
/// `permissions_route_degrades_on_malformed_toml`,
/// `permissions_route_degrades_gracefully_on_unresolvable_extends`.
pub(super) async fn permissions_at(dirs: &[PathBuf], name: &str) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid agent name" })),
        )
            .into_response();
    }
    let Some((path, _package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?e, agent = name, path = %path.display(), "permissions_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let (own_tools, own_permissions, extends_target, config_error) =
        parse_permission_sections(&raw);
    let own_scopes = effective_scopes(&own_tools, &own_permissions).unwrap_or_default();

    // Resolve the full `extends` chain so inherited scopes/tiers/autonomy are
    // visible (PM-7). Falls back to the single-file view on ANY resolution
    // failure — the pane still renders this agent's own declarations rather
    // than going dark over an unrelated ancestor's typo.
    let resolved = AgentConfig::by_name_in(dirs, name).ok();
    let (all_scopes, permissions, rbac) = match &resolved {
        Some(cfg) => (
            effective_scopes(&cfg.tools, &cfg.permissions).unwrap_or_default(),
            cfg.permissions.clone(),
            cfg.rbac.clone(),
        ),
        None => (
            own_scopes.clone(),
            own_permissions.clone(),
            RbacConfig::default(),
        ),
    };

    let scopes: Vec<Value> = all_scopes
        .iter()
        .map(|pattern| {
            let source = if own_scopes.contains(pattern) {
                "declared".to_string()
            } else if let Some(base) = &extends_target {
                format!("inherited:{base}")
            } else {
                // No `extends` on this file at all, yet the pattern isn't in
                // `own_scopes` — only possible if `extends` resolution itself
                // failed (see the doc comment); report it honestly rather
                // than asserting a provenance we cannot back.
                "declared".to_string()
            };
            json!({
                "pattern": pattern,
                "source": source,
                "enforced": true,
                // Qualifies the boolean above — see `SCOPE_ENFORCED_ON`'s doc
                // and the module doc's PM-6 bullet: this is NOT universal,
                // the `--direct`/subprocess dispatch path never reaches it.
                "enforced_on": SCOPE_ENFORCED_ON,
            })
        })
        .collect();

    let default_tier = permissions
        .default_tier
        .clone()
        .unwrap_or_else(|| rbac.effective_default_tier());
    let unauthenticated_tier = permissions
        .unauthenticated_tier
        .clone()
        .unwrap_or_else(|| rbac.effective_unauthenticated_tier());

    let mut body = json!({
        "scopes": scopes,
        "user_authority": {
            "value": permissions.user_authority,
            "enforced": false,
            "reason": "field reserved — #3074 not landed",
        },
        "tiers": {
            "default": tier_str(&default_tier),
            "unauthenticated": tier_str(&unauthenticated_tier),
            "enforced": false,
        },
        "autonomy": {
            "mode": autonomy_str(permissions.autonomy.unwrap_or_default()),
            "enforced": false,
            "reason": "declarative; see DOC-23",
        },
        "grants": permissions.grants.iter().map(|g| json!({
            "skill": g.skill,
            "mode": grant_mode_str(g.mode),
            "enforced": false,
        })).collect::<Vec<_>>(),
    });
    if let Some(err) = config_error {
        body["config_error"] = Value::String(err);
    } else if resolved.is_none() && extends_target.is_some() {
        // Own file parsed fine but the chain didn't resolve (missing base,
        // cycle, depth) — additive, non-normative field; `config_error`
        // stays reserved for THIS file's own parse failures, matching the
        // sibling routes.
        body["extends_warning"] = Value::String(
            "this agent's `extends` chain did not resolve; showing its own declarations only"
                .to_string(),
        );
    }
    (StatusCode::OK, Json(body)).into_response()
}

fn tier_str(tier: &ServiceTier) -> &'static str {
    match tier {
        ServiceTier::All => "all",
        ServiceTier::Analytics => "analytics",
        ServiceTier::ReadOnly => "read_only",
    }
}

fn autonomy_str(mode: crate::agents::permissions::AutonomyMode) -> &'static str {
    use crate::agents::permissions::AutonomyMode;
    match mode {
        AutonomyMode::AskFirst => "ask-first",
        AutonomyMode::LearnToAct => "learn-to-act",
    }
}

fn grant_mode_str(mode: crate::agents::permissions::GrantMode) -> &'static str {
    use crate::agents::permissions::GrantMode;
    match mode {
        GrantMode::Allow => "allow",
        GrantMode::Ask => "ask",
        GrantMode::Deny => "deny",
    }
}

/// Parse `[agent].extends`, `[tools]` and `[permissions]` out of a raw
/// `agent.toml`.
///
/// Why: Same partial-read rationale as the sibling routes' parse helpers — a
/// directory-package `agent.toml` omits `[system_prompt]`, so reading
/// through the full `AgentConfig` would reject it for the "own file" half of
/// this route's job (the "resolved chain" half separately uses the full
/// loader, which DOES handle packages — see `permissions_at`).
/// What: `(tools, permissions, extends_target, None)` on success; defaults
/// plus `Some(message)` when the file is not valid TOML.
/// Test: `parse_permission_sections_reads_all_three`,
/// `parse_permission_sections_reports_bad_toml`.
fn parse_permission_sections(
    raw: &str,
) -> (
    ToolsConfig,
    PermissionsConfig,
    Option<String>,
    Option<String>,
) {
    #[derive(serde::Deserialize, Default)]
    struct AgentIdentity {
        #[serde(default)]
        extends: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        agent: AgentIdentity,
        #[serde(default)]
        tools: ToolsConfig,
        #[serde(default)]
        permissions: PermissionsConfig,
    }
    match toml::from_str::<Partial>(raw) {
        Ok(p) => (p.tools, p.permissions, p.agent.extends, None),
        Err(e) => (
            ToolsConfig::default(),
            PermissionsConfig::default(),
            None,
            Some(e.to_string()),
        ),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parse_permission_sections_reads_all_three() {
        let raw = r#"
[agent]
name = "cto-assistant"
extends = "assistant"

[tools]
scopes = ["memory.read"]

[permissions]
scopes = ["google.gmail.*"]
user_authority = true
"#;
        let (tools, permissions, extends, err) = parse_permission_sections(raw);
        assert!(err.is_none());
        assert_eq!(tools.scopes, Some(vec!["memory.read".to_string()]));
        assert_eq!(permissions.scopes, Some(vec!["google.gmail.*".to_string()]));
        assert!(permissions.user_authority);
        assert_eq!(extends.as_deref(), Some("assistant"));
    }

    #[test]
    fn parse_permission_sections_tolerates_package_agent_toml() {
        let (tools, permissions, extends, err) =
            parse_permission_sections("[agent]\nname = \"x\"\n");
        assert!(err.is_none());
        assert!(tools.scopes.is_none());
        assert_eq!(permissions, PermissionsConfig::default());
        assert!(extends.is_none());
    }

    #[test]
    fn parse_permission_sections_reports_bad_toml() {
        let (_, _, _, err) = parse_permission_sections("not = = toml");
        assert!(err.is_some());
    }

    #[test]
    fn tier_str_matches_serde_spelling() {
        assert_eq!(tier_str(&ServiceTier::All), "all");
        assert_eq!(tier_str(&ServiceTier::Analytics), "analytics");
        assert_eq!(tier_str(&ServiceTier::ReadOnly), "read_only");
    }
}
