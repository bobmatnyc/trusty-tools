//! `GET /api/agents/:name/skills` — the agent's capability set, one skill per
//! tool (#3933, DOC-57 §5.7).
//!
//! Why: The Phase-1 Skills pane (#3945) had no route to read, so it synthesised
//! cards client-side by chopping `[tools].allow` globs on `_` and badged every
//! one `synthetic` — honest about being a placeholder, but it showed the user
//! prefix fragments rather than capabilities and could not say whether anything
//! resolved. This route replaces that guess with the resolved catalog: real
//! names, the tool each one wraps, whether this agent is granted it, and what
//! credential it needs.
//! What: [`agent_skills_route`] is the axum shim; [`skills_at`] is the testable
//! core taking the agents-dir list explicitly (same injected-dependency
//! convention as `agent_stores::stores_at`). Every skill in the catalog is
//! returned with a `granted` flag rather than only the granted subset, so the
//! Phase-3 editor can render the full choice without a second route.
//!
//! **Honesty rules this route holds to** (#3945's discipline, restated):
//! - `granted` is computed by the SAME `match_any_glob` the dispatch gate uses,
//!   against the SAME compiled patterns, from a catalog built the SAME way
//!   (built-in + authored overlay) — it is not a re-implementation, and it does
//!   not read a narrower catalog than the runtime it describes.
//! - A credential is reported as present only when it is an environment
//!   variable this process can actually read. An OAuth grant reports its
//!   requirement with `configured: null` — unknown, never "yes".
//! - An allow-pattern matching no catalog tool is surfaced in
//!   `unmatched_patterns`, not dropped: it may still resolve to a live-
//!   discovered MCP tool at dispatch time, and saying so is more useful than
//!   either hiding it or claiming it is broken.
//! Test: `super::tests::agent_skills`.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::agents::{SkillsConfig, ToolsConfig};
use crate::ctrl::pm_task::match_any_glob;
use crate::skills::manifest::{SkillCatalog, SkillManifest, effective_tool_patterns};

/// `GET /api/agents/:name/skills` — HTTP entry point.
///
/// Why/What: see the module doc. The agents-dir list and the skill-source root
/// are discovered here and threaded into [`skills_at`] so tests can substitute
/// temp directories. The root mirrors the dispatch call sites
/// (`ctrl::pm_task::dispatch::persona` passes its `project_path`;
/// `runtime::subagent_mode` passes its `cwd`), so the route resolves authored
/// manifests from the same sources dispatch will.
/// Test: `super::tests::agent_skills::skills_route_reports_granted_skills_with_human_names`.
pub(super) async fn agent_skills_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    skills_at(
        &crate::agents::agents_dir_candidates(),
        &name,
        &project_root,
    )
    .await
}

/// Core skill-resolution logic against an explicit agents-dir list.
///
/// Why: Same testability rationale as `agent_stores::stores_at`, and the same
/// degradation posture — malformed TOML is NOT a `500`. It degrades to "no
/// grants" plus a `config_error` field, so a hand-edited file breaking the
/// `[tools]` table still lets the panel render the rest of the agent.
/// What: `400` for an invalid name, `404` for an unknown agent, `500` only when
/// the resolved config file cannot be read.
/// Test: `skills_route_reports_granted_skills_with_human_names`, `skills_route_unknown_agent_404`,
/// `skills_route_degrades_on_malformed_toml`,
/// `skills_route_surfaces_unresolved_skill_ids`,
/// `skills_route_resolves_an_authored_only_skill_id`.
pub(super) async fn skills_at(dirs: &[PathBuf], name: &str, project_root: &Path) -> Response {
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
            tracing::warn!(?e, agent = name, path = %path.display(), "skills_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let (tools, skills, config_error) = parse_capability_sections(&raw);
    // The catalog MUST be built exactly as the dispatch paths build it
    // (`persona.rs`, `subagent_mode.rs`), authored overlay included. Reading a
    // built-in-only catalog here would make this route disagree with the runtime
    // it reports on: an authored-only skill id would be listed as `unresolved`
    // while dispatch resolved and granted it, and `web-search.md`'s authored
    // name would never reach the pane. A display path that contradicts the
    // enforcement path is the audit-surface form of the same
    // capability-model-holds-on-one-path defect the two dispatch sites were
    // wired together to avoid.
    let catalog =
        SkillCatalog::builtin().with_authored(crate::skills::manifest::authored::load_from_paths(
            &crate::skills::sources::SkillSourceRegistry::load(project_root).resolved_paths(),
        ));
    let (patterns, unresolved) =
        effective_tool_patterns(tools.allow.as_ref(), skills.allow.as_ref(), &catalog);
    let granted_ids = granted_skill_ids(&catalog, patterns.as_deref(), skills.allow.as_ref());

    let mut cards: Vec<Value> = catalog
        .manifests()
        .iter()
        .map(|m| render_skill(m, granted_ids.contains(&m.id)))
        .collect();
    cards.extend(
        derived_skills(&catalog, patterns.as_deref())
            .iter()
            .map(|m| render_skill(m, true)),
    );
    let granted_count = cards
        .iter()
        .filter(|c| c["granted"] == Value::Bool(true))
        .count();

    let mut body = json!({
        "skills": cards,
        "granted_count": granted_count,
        "unresolved": unresolved
            .iter()
            .map(|id| json!({
                "id": id,
                "reason": "no skill with this id is built in or authored in any skill source",
            }))
            .collect::<Vec<_>>(),
        "unmatched_patterns": unmatched_patterns(&catalog, patterns.as_deref()),
        // Deny-on-absent polarity, stated rather than implied: `None` here is a
        // persona with no capability declaration at all, which grants nothing.
        "declares_capability": patterns.is_some(),
    });
    if let Some(err) = config_error {
        body["config_error"] = Value::String(err);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// Ids of the skills this agent is actually granted.
///
/// Why: `granted` must agree with dispatch, so it is computed with the SAME
/// `match_any_glob` against the SAME compiled patterns rather than by a second
/// rule that can drift. Tool-less skills have nothing to match, so they are
/// granted exactly when `[skills].allow` names them — which is also why they
/// need their own clause instead of falling out of the glob test as `false`.
/// What: Returns the granted ids. `patterns == None` (no capability declared)
/// grants nothing, matching the persona path's `else` arm.
/// Test: `skills_route_reports_granted_skills_with_human_names`,
/// `granted_ids_include_tool_less_skills_named_by_id`.
fn granted_skill_ids(
    catalog: &SkillCatalog,
    patterns: Option<&[String]>,
    skills_allow: Option<&Vec<String>>,
) -> std::collections::BTreeSet<String> {
    let mut granted = std::collections::BTreeSet::new();
    if let Some(patterns) = patterns {
        for manifest in catalog.manifests() {
            if let Some(tool) = manifest.tool()
                && match_any_glob(tool, patterns)
            {
                granted.insert(manifest.id.clone());
            }
        }
    }
    if let Some(ids) = skills_allow {
        for id in ids {
            if catalog.get(id).is_some_and(|m| m.tool().is_none()) {
                granted.insert(id.clone());
            }
        }
    }
    granted
}

/// Derived 1:1 skills for exactly-named tools the catalog does not know.
///
/// Why: DOC-57 C-04.3 — every tool in the effective set maps to exactly one
/// skill, so the pane has no gaps. An agent granting a live-discovered MCP tool
/// by its exact name (`granola_list_meetings`) would otherwise appear to hold no
/// capability at all, which is a worse lie than an unnamed card. Deriving here
/// keeps the invariant without inventing prose: `SkillManifest::derived` carries
/// a title-cased name and NOTHING else, and the card is badged `derived` so the
/// naming gap stays visible and measurable (S-10).
///
/// A GLOB pattern is deliberately NOT derived. `granola_*` names no single tool,
/// so there is nothing to wrap 1:1; it is reported in `unmatched_patterns`
/// instead, which says what it is rather than fabricating a card per guess.
/// What: One derived manifest per exact-name pattern with no catalog skill.
/// Test: `derived_skills_wrap_an_exactly_named_unknown_tool`,
/// `derived_skills_ignore_globs`.
fn derived_skills(catalog: &SkillCatalog, patterns: Option<&[String]>) -> Vec<SkillManifest> {
    let Some(patterns) = patterns else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    patterns
        .iter()
        .filter(|p| !p.contains('*'))
        .filter(|p| catalog.skill_for_tool(p).is_none())
        .filter(|p| seen.insert((*p).clone()))
        .map(|p| SkillManifest::derived(p))
        .collect()
}

/// Allow-patterns that no catalog tool matches.
///
/// Why: DOC-57 S-11's polarity applied to the other direction — a grant that
/// resolves to nothing must be visible. Crucially the reason is honest about
/// WHY: live-discovered MCP tools are not in the compile-time catalog, so
/// `granola_*` matching nothing here does NOT mean the agent lacks it.
/// What: One entry per GLOB pattern with no matching wrapped tool. Exact names
/// are excluded because they get a derived card instead (`derived_skills`) —
/// reporting them in both places would double-count one grant.
/// Test: `skills_route_reports_unmatched_patterns_honestly`,
/// `unmatched_patterns_excludes_exact_names_that_get_a_derived_card`.
fn unmatched_patterns(catalog: &SkillCatalog, patterns: Option<&[String]>) -> Vec<Value> {
    let Some(patterns) = patterns else {
        return Vec::new();
    };
    patterns
        .iter()
        .filter(|p| p.contains('*'))
        .filter(|p| {
            !catalog
                .manifests()
                .iter()
                .filter_map(SkillManifest::tool)
                .any(|tool| match_any_glob(tool, std::slice::from_ref(p)))
        })
        .map(|p| {
            json!({
                "pattern": p,
                "reason": "no skill in the catalog wraps a tool matching this pattern; it may \
                           still resolve to an MCP tool discovered at dispatch time, which is \
                           wrapped in a derived skill then",
            })
        })
        .collect()
}

/// Render one manifest as the pane's card payload.
///
/// Why/What: Keeps the JSON shape in one place. `provider.configured` is
/// tri-state on purpose: `true`/`false` when an environment variable backs the
/// credential and this process can read it, `null` when the credential is an
/// OAuth grant or MCP wiring that this endpoint does not verify. Rendering
/// `null` as "not verified" is the whole point — a `false` there would assert a
/// check nobody ran.
/// Test: `skill_card_reports_env_credential_state`.
fn render_skill(manifest: &SkillManifest, granted: bool) -> Value {
    let provider = manifest.provider.map(|p| {
        let configured = p.env_var.map(|var| {
            std::env::var(var)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        });
        json!({
            "provider": p.provider,
            "requirement": p.requirement,
            "env_var": p.env_var,
            "configured": configured,
        })
    });
    json!({
        "id": manifest.id,
        "name": manifest.name,
        "description": manifest.description,
        "kind": manifest.kind,
        "origin": manifest.origin,
        "granted": granted,
        "tools": manifest.tools,
        "provider": provider,
    })
}

/// Parse just `[tools]` and `[skills]` out of a raw `agent.toml`.
///
/// Why: Reading the whole file through `AgentConfig` would require `[llm]` and
/// `[system_prompt]` to be present and valid — true for a flat agent but NOT
/// for a directory package, whose `agent.toml` deliberately omits
/// `system_prompt.content` (it lives in `persona.md`). Same partial-read
/// precedent as `agent_stores::parse_stores`.
/// What: `(tools, skills, None)` on success; defaults plus `Some(message)` when
/// the file is not valid TOML.
/// Test: `parse_capability_sections_reads_both`,
/// `parse_capability_sections_reports_bad_toml`.
fn parse_capability_sections(raw: &str) -> (ToolsConfig, SkillsConfig, Option<String>) {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        tools: ToolsConfig,
        #[serde(default)]
        skills: SkillsConfig,
    }
    match toml::from_str::<Partial>(raw) {
        Ok(p) => (p.tools, p.skills, None),
        Err(e) => (
            ToolsConfig::default(),
            SkillsConfig::default(),
            Some(e.to_string()),
        ),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parse_capability_sections_reads_both() {
        let raw = "[agent]\nname = \"izzie\"\n\n[tools]\nallow = [\"git_*\"]\n\n[skills]\nallow = [\"mta-train-time\"]\n";
        let (tools, skills, err) = parse_capability_sections(raw);
        assert!(err.is_none());
        assert_eq!(tools.allow.unwrap(), vec!["git_*".to_string()]);
        assert_eq!(skills.allow.unwrap(), vec!["mta-train-time".to_string()]);
    }

    #[test]
    fn parse_capability_sections_tolerates_package_agent_toml() {
        // A package `agent.toml` has no `[system_prompt]`; parsing must not
        // require one (see this fn's doc comment).
        let (tools, skills, err) = parse_capability_sections("[agent]\nname = \"x\"\n");
        assert!(err.is_none());
        assert!(tools.allow.is_none());
        assert!(skills.allow.is_none());
    }

    #[test]
    fn parse_capability_sections_reports_bad_toml() {
        let (_, _, err) = parse_capability_sections("this is not = = toml");
        assert!(err.is_some());
    }

    #[test]
    fn granted_ids_are_empty_when_nothing_is_declared() {
        // Deny-on-absent: no `[tools].allow` and no `[skills].allow` grants
        // nothing, which must not be confused with "unrestricted".
        let catalog = SkillCatalog::builtin();
        assert!(granted_skill_ids(&catalog, None, None).is_empty());
    }

    #[test]
    fn granted_ids_track_the_dispatch_gate() {
        let catalog = SkillCatalog::builtin();
        let patterns = vec!["get_train_schedule".to_string(), "git_*".to_string()];
        let granted = granted_skill_ids(&catalog, Some(&patterns), None);
        assert!(granted.contains("mta-train-time"));
        assert!(
            granted.contains("git-status"),
            "trailing-* glob is honoured"
        );
        assert!(!granted.contains("gmail-search"));
    }

    #[test]
    fn granted_ids_include_tool_less_skills_named_by_id() {
        let catalog = SkillCatalog::builtin();
        let allow = vec!["handoff-protocol".to_string()];
        let granted = granted_skill_ids(&catalog, Some(&[]), Some(&allow));
        assert!(granted.contains("handoff-protocol"));
    }

    #[test]
    fn unmatched_patterns_flags_a_pattern_no_skill_wraps() {
        let catalog = SkillCatalog::builtin();
        let patterns = vec!["granola_*".to_string(), "get_weather".to_string()];
        let flagged = unmatched_patterns(&catalog, Some(&patterns));
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0]["pattern"], "granola_*");
    }

    #[test]
    fn unmatched_patterns_excludes_exact_names_that_get_a_derived_card() {
        // One grant, one report. An exact name the catalog does not know gets a
        // derived card; listing it as "unmatched" too would make one grant look
        // like a capability AND a problem simultaneously.
        let catalog = SkillCatalog::builtin();
        let patterns = vec!["granola_list_meetings".to_string()];
        assert!(unmatched_patterns(&catalog, Some(&patterns)).is_empty());
        assert_eq!(derived_skills(&catalog, Some(&patterns)).len(), 1);
    }

    #[test]
    fn derived_skills_wrap_an_exactly_named_unknown_tool() {
        let catalog = SkillCatalog::builtin();
        let patterns = vec![
            "granola_list_meetings".to_string(),
            "get_weather".to_string(),
        ];
        let derived = derived_skills(&catalog, Some(&patterns));
        assert_eq!(derived.len(), 1, "a known tool must not be derived twice");
        assert_eq!(derived[0].name, "Granola List Meetings");
        assert_eq!(derived[0].tools, vec!["granola_list_meetings".to_string()]);
        assert!(
            derived[0].description.is_empty(),
            "a derived skill invents no prose"
        );
    }

    #[test]
    fn derived_skills_ignore_globs() {
        // A glob names no single tool, so there is nothing to wrap 1:1.
        let catalog = SkillCatalog::builtin();
        let patterns = vec!["granola_*".to_string()];
        assert!(derived_skills(&catalog, Some(&patterns)).is_empty());
    }

    #[test]
    fn derived_skills_are_empty_when_nothing_is_declared() {
        assert!(derived_skills(&SkillCatalog::builtin(), None).is_empty());
    }

    #[test]
    fn skill_card_reports_env_credential_state() {
        let catalog = SkillCatalog::builtin();
        // OAuth-backed: `configured` must stay null — unknown, never asserted.
        let gmail = catalog.skill_for_tool("search_gmail_messages").unwrap();
        let card = render_skill(gmail, true);
        assert_eq!(card["provider"]["configured"], Value::Null);
        assert_eq!(card["provider"]["provider"], "Google Workspace");
        assert_eq!(card["granted"], true);

        // Env-backed: `configured` is a real boolean derived from the process
        // environment. Its VALUE depends on the machine, so assert only that a
        // boolean — not null — is produced.
        let mta = catalog.skill_for_tool("get_train_schedule").unwrap();
        let card = render_skill(mta, false);
        assert!(card["provider"]["configured"].is_boolean());
        assert_eq!(card["provider"]["env_var"], "MTA_API_KEY");
    }

    #[test]
    fn skill_card_carries_one_tool_and_a_human_name() {
        let catalog = SkillCatalog::builtin();
        let mta = catalog.skill_for_tool("get_train_schedule").unwrap();
        let card = render_skill(mta, true);
        assert_eq!(card["name"], "MTA Train Time");
        assert_eq!(card["tools"], json!(["get_train_schedule"]));
        assert_eq!(card["origin"]["kind"], "builtin");
    }
}
