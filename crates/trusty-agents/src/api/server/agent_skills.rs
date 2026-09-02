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
//! - `granted` is computed by the SAME predicate the dispatch gate uses —
//!   `persona_surface_grants_tool` (#4520/#4054): allow-globs with the L0
//!   literal-only rule and the exfil strip, not a bare `match_any_glob` — against
//!   the SAME compiled patterns, from a catalog built the SAME way (built-in +
//!   authored overlay). It is not a re-implementation, and it does not read a
//!   narrower catalog than the runtime it describes.
//! - A credential is reported as present only when it is an environment
//!   variable this process can actually read. An OAuth grant reports its
//!   requirement with `configured: null` — unknown, never "yes".
//! - An allow-pattern matching no catalog tool is surfaced in
//!   `unmatched_patterns`, not dropped: it may still resolve to a live-
//!   discovered MCP tool at dispatch time, and saying so is more useful than
//!   either hiding it or claiming it is broken.
//! - #3987: a `[tools].scopes` pattern that can match NO reachable dotted
//!   scope is surfaced in `dead_scope_patterns` — the scope-side analogue of
//!   `unmatched_patterns`, and for the same reason. A dead scope grant denies
//!   every scoped tool it was meant to permit while looking exactly like "this
//!   agent has no tools"; a panel that renders `granted: true` cards for tools
//!   the dispatch gate will drop is the audit-surface form of that same lie.
//!
//! **#4025 — function-skill groups.** The response is ADDITIVE only: every field
//! a pre-#4022 consumer reads keeps its name, type and meaning. Function skills
//! (#4022) appear as ordinary `skills[]` cards with `kind: "function"`, plus a
//! top-level `groups[]` index so #4024's pane can render group headers without
//! re-deriving membership from the catalog. Both are built from ONE computation
//! ([`function_groups`]) rather than two, so a card and its group entry can never
//! report different grant states. `granted_state` is the tri-state
//! (`"all"`/`"some"`/`"none"` of the members) and the boolean `granted` stays
//! what it always was — for a bundle it is `granted_state == "all"`, the
//! conservative answer an existing consumer gets for free. `granted_count`
//! deliberately EXCLUDES function cards: it counts capabilities, and a bundle is
//! not a capability of its own — counting it would inflate the number the pane
//! shows without the agent gaining anything.
//!
//! **#4024 — a group's credential requirements are a SET, never a verdict.** A
//! bundle carries no `provider` of its own (its card keeps `provider: null`),
//! but its members do, and they need not agree. `groups[].providers` is the
//! DISTINCT requirements across the members, each in the same shape and with the
//! same tri-state `configured` a leaf card carries. Two members needing two
//! different credentials therefore produce two entries the pane renders side by
//! side; nothing is collapsed into a single "configured"/"not configured" verdict
//! for the group, because averaging a divergence is the quiet form of the lie
//! #3945 exists to prevent. The dotted-SCOPE axis (`google.gmail.*` vs
//! `google.tasks.*`) is not part of the manifest's provider model and is not
//! claimed here — `dead_scope_patterns` reports it separately.
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
// `match_any_glob` is the CATALOG-coverage matcher (dead-pattern diagnostic);
// `persona_surface_grants_tool` is the DISPATCH-gate matcher used for `granted`
// (#4520/#4054). The two answer different questions — see their call sites.
use crate::ctrl::pm_task::match_any_glob;
use crate::ctrl::pm_task::tool_authz::persona_surface_grants_tool;
use crate::skills::manifest::{ProviderReq, SkillCatalog, SkillManifest, effective_tool_patterns};
use crate::tools::registry::dead_scope;
use crate::tools::registry::scope::ScopePattern;

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
    // #4025: one computation feeds both the function cards and `groups[]`.
    let groups = function_groups(&catalog, &granted_ids);

    let mut cards: Vec<Value> = catalog
        .manifests()
        .iter()
        .map(|m| {
            let group = groups.iter().find(|g| g.id == m.id);
            render_skill(m, granted_ids.contains(&m.id), group)
        })
        .collect();
    cards.extend(
        derived_skills(&catalog, patterns.as_deref())
            .iter()
            .map(|m| render_skill(m, true, None)),
    );
    // A bundle is not a capability, so it does not move this count (#4025).
    let granted_count = cards
        .iter()
        .filter(|c| c["granted"] == Value::Bool(true) && c["kind"] != "function")
        .count();

    let mut body = json!({
        "skills": cards,
        "granted_count": granted_count,
        "groups": groups.iter().map(FunctionGroup::to_json).collect::<Vec<_>>(),
        "unresolved": unresolved
            .iter()
            .map(|id| json!({
                "id": id,
                "reason": "no skill with this id is built in or authored in any skill source",
            }))
            .collect::<Vec<_>>(),
        "unmatched_patterns": unmatched_patterns(&catalog, patterns.as_deref()),
        // #3987 (option C): scope grants that can never match anything.
        "dead_scope_patterns": dead_scope_cards(tools.scopes.as_ref()),
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
/// `persona_surface_grants_tool` predicate (#4520/#4054 — allow-globs plus the
/// L0 literal-only rule and the exfil strip) against the SAME compiled patterns
/// rather than by a second rule that can drift; a bare `match_any_glob` would
/// mark an L0 or exfil skill-tool granted under `*` though the real gate denies
/// it. Tool-less skills have nothing to match, so they are granted exactly when
/// `[skills].allow` names them — which is also why they need their own clause
/// instead of falling out of the glob test as `false`.
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
                && persona_surface_grants_tool(tool, patterns)
            {
                granted.insert(manifest.id.clone());
            }
        }
    }
    if let Some(ids) = skills_allow {
        for id in ids {
            // #4022: a FUNCTION skill is deliberately excluded from this clause.
            // It is tool-less, so it would otherwise be granted merely by being
            // named — asserting a grant this route never verified. Its state is
            // derived from its members instead (`function_groups`), which is the
            // only answer that stays true when a member fails to resolve.
            if catalog
                .get(id)
                .is_some_and(|m| m.tool().is_none() && !m.is_function())
            {
                granted.insert(id.clone());
            }
        }
    }
    granted
}

/// One function skill resolved against this agent's grants (#4022/#4025).
///
/// Why: The pane needs "7 of 10 granted", which is neither a boolean nor
/// derivable from the bundle id alone. Computing it ONCE here — from the
/// `granted_ids` set the leaf cards already use — is what stops the group header
/// and its member cards from disagreeing, which is the display-layer form of the
/// same drift `granted_skill_ids` exists to prevent against dispatch.
/// What: The bundle's identity plus its declared members and the subset of them
/// this agent holds.
/// Test: `skills_route_surfaces_function_skill_tri_state`.
struct FunctionGroup {
    id: String,
    name: String,
    description: String,
    members: Vec<String>,
    granted_members: Vec<String>,
    /// #4024: the DISTINCT credential requirements across the members.
    ///
    /// Why: see the module doc — a bundle has no credential of its own, and its
    /// members' need not agree. Carrying the set (rather than a rolled-up
    /// boolean) is what makes a divergence renderable instead of averaged away.
    /// Test: `group_providers_list_every_distinct_member_requirement`.
    providers: Vec<ProviderReq>,
}

impl FunctionGroup {
    /// `"all"` / `"some"` / `"none"` of the members are granted.
    ///
    /// Why/What: A memberless bundle reports `"none"` rather than the vacuous
    /// `"all"` — claiming a group is fully granted when it contains nothing is
    /// the fabrication #3945 forbids.
    /// Test: `skills_route_surfaces_function_skill_tri_state`.
    fn state(&self) -> &'static str {
        if self.members.is_empty() || self.granted_members.is_empty() {
            "none"
        } else if self.granted_members.len() == self.members.len() {
            "all"
        } else {
            "some"
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "description": self.description,
            "members": self.members,
            "granted_members": self.granted_members,
            "granted_state": self.state(),
            "providers": self.providers.iter().map(provider_json).collect::<Vec<_>>(),
        })
    }
}

/// One credential requirement in the card/group wire shape.
///
/// Why: `configured` is evaluated from the process environment in exactly one
/// place, so a leaf card and the group header above it can never disagree about
/// the same credential. It stays tri-state: `true`/`false` only when an
/// environment variable backs the requirement, `null` when it is an OAuth grant
/// or MCP wiring this endpoint does not verify — a `false` there would assert a
/// check nobody ran.
/// What: `{ provider, requirement, env_var, configured }`.
/// Test: `skill_card_reports_env_credential_state`,
/// `group_providers_list_every_distinct_member_requirement`.
fn provider_json(p: &ProviderReq) -> Value {
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
}

/// The distinct credential requirements the members of a bundle carry.
///
/// Why (#4024, S-16): a bundle is granted as one line, so the pane must be able
/// to state what that line NEEDS. Members can diverge — nothing in the manifest
/// model forces one bundle to one provider — and the honest rendering of a
/// divergence is to show both, not to pick one or to collapse them into a single
/// verdict. Distinctness is by the WHOLE requirement, not by provider name: two
/// requirements that differ in text differ in what the operator must do, and
/// hiding the second behind a shared name is the same lie one level down.
/// What: Requirements in member declaration order, first occurrence kept. A
/// member no manifest resolves contributes nothing (it also cannot be granted),
/// and members needing no credential contribute nothing — an empty vec means
/// "no member states a requirement", never "verified".
/// Test: `group_providers_list_every_distinct_member_requirement`,
/// `group_providers_are_empty_when_no_member_needs_a_credential`.
fn distinct_member_providers(catalog: &SkillCatalog, members: &[String]) -> Vec<ProviderReq> {
    let mut distinct: Vec<ProviderReq> = Vec::new();
    for member in members {
        if let Some(p) = catalog.get(member).and_then(|m| m.provider)
            && !distinct.contains(&p)
        {
            distinct.push(p);
        }
    }
    distinct
}

/// Resolve every function skill in the catalog against this agent's grants.
///
/// Why: See [`FunctionGroup`] — one computation, two render sites.
/// What: One entry per `kind: Function` manifest, members in declaration order.
/// A member id no manifest resolves is still LISTED (it is what the bundle
/// declares) but can never appear in `granted_members`, so an unresolvable
/// member drags the state down to `"some"`/`"none"` rather than being hidden.
/// Test: `skills_route_surfaces_function_skill_tri_state`,
/// `function_group_never_reports_all_when_a_member_is_missing`.
fn function_groups(
    catalog: &SkillCatalog,
    granted_ids: &std::collections::BTreeSet<String>,
) -> Vec<FunctionGroup> {
    catalog
        .manifests()
        .iter()
        .filter(|m| m.is_function())
        .map(|m| FunctionGroup {
            id: m.id.clone(),
            name: m.name.clone(),
            description: m.description.clone(),
            members: m.members.clone(),
            granted_members: m
                .members
                .iter()
                .filter(|id| granted_ids.contains(*id))
                .cloned()
                .collect(),
            // #4024: what this one grant NEEDS, as a set of distinct requirements.
            providers: distinct_member_providers(catalog, &m.members),
        })
        .collect()
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

/// Declared `[tools].scopes` patterns that can match no reachable scope
/// (#3987, option C).
///
/// Why: this route is the config-introspection surface the GUI/CLI reads, and
/// until now it could show an agent a page full of `granted: true` gworkspace
/// cards that the dispatch-time scope gate silently drops — the base
/// `assistant`'s `google.read` grants literally nothing. Surfacing the dead
/// pattern here is what lets a client say "this agent has dead scope grants"
/// instead of leaving the user to conclude the tools simply do not work.
///
/// Honesty rules, matching the rest of this module:
/// - The reachable set here is the STATIC vocabulary only
///   (`dead_scope::reachable_scopes` with no live registry) — this route runs
///   in the API process and has no built tool registry to consult. That is the
///   conservative direction: the static vocabulary is a superset of what an
///   offline endpoint would contribute for namespaces we know, and
///   `dead_scope`'s namespace guard declines to judge namespaces we do not.
///   A pattern reported here is dead under ANY registry state.
/// - Scopes are read from the agent's OWN file, without resolving `extends`
///   (same partial-read posture as the rest of this route — see
///   `parse_capability_sections`). An agent that declares no `[tools].scopes`
///   of its own reports nothing here even if it inherits a dead pattern from
///   its base; the dispatch-time `warn!` in
///   `ctrl::pm_task::dispatch::persona` sees the resolved set and covers that
///   case. Under-reporting is the correct failure direction for an audit
///   surface — it never claims a working grant is broken.
/// What: one `{ pattern, nearest, reason }` object per dead pattern.
/// Test: `dead_scope_cards_flags_the_google_read_pattern`,
/// `dead_scope_cards_ignores_live_family_patterns`,
/// `super::tests::agent_skills::skills_route_reports_dead_scope_patterns`.
fn dead_scope_cards(scopes: Option<&Vec<String>>) -> Vec<Value> {
    let Some(scopes) = scopes else {
        return Vec::new();
    };
    let patterns: Vec<ScopePattern> = scopes.iter().cloned().map(ScopePattern::new).collect();
    let reachable = dead_scope::reachable_scopes(std::iter::empty::<String>());
    dead_scope::dead_scope_patterns(&patterns, &reachable)
        .into_iter()
        .map(|d| {
            json!({
                "pattern": d.pattern,
                "nearest": d.nearest,
                "reason": format!(
                    "no reachable tool advertises a scope matching this pattern, so every \
                     scoped tool it was meant to grant is denied at dispatch — {}",
                    d.describe(),
                ),
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
///
/// #4025: `members` and `granted_state` are present on EVERY card, not just
/// function ones. A consumer that has to check the kind before it knows whether
/// a field exists writes the branch wrong once and then renders a leaf skill as
/// an empty group; a uniform shape costs two empty arrays and removes the
/// branch. For a leaf, `members` is empty and `granted_state` restates `granted`.
/// For a bundle, `granted` is `granted_state == "all"` — the boolean an existing
/// consumer already reads keeps its conservative meaning.
///
/// #4024: a bundle card's `provider` stays `null`, because a bundle has no single
/// credential to name. Its members' requirements live in `groups[].providers` as
/// a SET — see [`distinct_member_providers`] — so a divergence is rendered rather
/// than collapsed into one field that could only be wrong.
/// Test: `skill_card_reports_env_credential_state`,
/// `skills_route_surfaces_function_skill_tri_state`.
fn render_skill(manifest: &SkillManifest, granted: bool, group: Option<&FunctionGroup>) -> Value {
    let provider = manifest.provider.as_ref().map(provider_json);
    let granted_state = match group {
        Some(g) => g.state(),
        None if granted => "all",
        None => "none",
    };
    json!({
        "id": manifest.id,
        "name": manifest.name,
        "description": manifest.description,
        "kind": manifest.kind,
        "origin": manifest.origin,
        "granted": group.map_or(granted, |g| g.state() == "all"),
        "tools": manifest.tools,
        "provider": provider,
        "members": group.map(|g| g.members.clone()).unwrap_or_default(),
        "granted_members": group.map(|g| g.granted_members.clone()).unwrap_or_default(),
        "granted_state": granted_state,
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

// #4024: the unit tests moved to `tests/agent_skills_unit_tests.rs` — still a
// child module of this one (so they keep private access), just not counting
// against this file's production SLOC cap. Nothing else about them changed.
#[cfg(test)]
#[path = "tests/agent_skills_unit_tests.rs"]
mod unit_tests;
