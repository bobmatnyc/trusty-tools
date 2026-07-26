//! Skill manifests: the one-skill-per-tool wrapping layer (#3933, DOC-57 §5).
//!
//! Why: Owner directive, 2026-07-25 — *"One skill per tool. There can be other
//! skills without tools, but each [tool] needs an accompanying skill"*, refining
//! the earlier *"gworkspace is a Google Workspace skill, MTA Train Time is that
//! skill"* naming note. That resolves DOC-57 §12 OQ-2 in favour of **1:1
//! granularity with human-readable names**, not per-family grouping. Before this
//! module a skill was prose injected as a `# Skill: <name>` prompt layer with no
//! binding to any tool at all: `.trusty-agents/skills/web-search.md` documented
//! `web_search` while `[tools].allow` granted it separately, so deleting either
//! half left the other silently lying. A manifest is that missing binding.
//! What: [`SkillManifest`] (one human-named capability wrapping zero or one
//! tool), [`SkillCatalog`] (the resolved set: built-in manifests, overridden by
//! authored `.md` frontmatter), and [`effective_tool_patterns`] — the
//! compile-down that turns `[skills].allow` into the exact tool names fed to the
//! EXISTING `filter_persona_tool_names` gate. No enforcement moves: this layer
//! sits in front of the three gates (allow-glob, RBAC tier, scope), so a
//! resolution bug can narrow capability but can never widen it.
//! Test: `skills/manifest/tests.rs`.

pub mod authored;
pub mod builtin;

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;

/// Which pane a skill renders in (DOC-57 §5.3).
///
/// Why: `kind` is the only mechanism routing capability between the Knowledge
/// and Skills panes; neither pane hardcodes tool names.
/// What: `Action` (Skills pane), `Knowledge` (Knowledge pane), `System`
/// (Skills pane, collapsed under "System").
/// Test: `knowledge_kind_routes_out_of_the_skills_pane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillKind {
    Action,
    Knowledge,
    System,
}

/// A provider/credential a skill's tool depends on.
///
/// Why: #3945's honesty discipline — never imply a capability works. A Gmail
/// skill whose Google account was never authorized must SAY so rather than
/// render as an available card. Recording the requirement next to the manifest
/// is what lets the pane state it without the backend guessing.
/// What: A human sentence plus, when the credential is a plain environment
/// variable, its name — the one thing this process can actually verify. OAuth
/// grants and MCP-service wiring are NOT verified here; `env_var: None` means
/// "requirement stated, status not checked", never "configured".
/// Test: `provider_env_presence_is_reported_only_for_env_backed_credentials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderReq {
    /// Human provider name, e.g. `"Google Workspace"`, `"MTA"`.
    pub provider: &'static str,
    /// One sentence naming what the operator must configure.
    pub requirement: &'static str,
    /// Environment variable carrying the credential, when there is one.
    pub env_var: Option<&'static str>,
}

/// A compile-time built-in manifest row.
///
/// Why: Authoring ~170 Markdown files before the Skills pane can render a
/// single honest card would block the GUI on a content project. A `const` table
/// of `&'static str` gives every registered tool a named home today, at zero
/// runtime cost, and keeps the coverage invariant mechanically checkable.
/// What: One row = one skill. `tool: None` is a first-class **tool-less skill**
/// (pure procedure/knowledge guidance with no executable member), not an edge
/// case. `tool: Some(..)` names exactly one tool — the 1:1 rule.
/// Test: `builtin_rows_wrap_at_most_one_tool`, `builtin_ids_are_unique`.
#[derive(Debug, Clone, Copy)]
pub struct SkillDef {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub tool: Option<&'static str>,
    pub kind: SkillKind,
    pub provider: Option<&'static ProviderReq>,
}

/// Terse constructor for a tool-wrapping built-in row.
///
/// Why: The catalog is ~170 rows; a struct literal per row would triple the
/// file count for no added clarity. A `const fn` keeps each row on one line and
/// still produces a compile-time constant.
/// What: Builds a [`SkillDef`] wrapping exactly one tool.
/// Test: Exercised by every `builtin::*` table via `builtin_rows_claim_each_tool_once`.
pub const fn tool_skill(
    id: &'static str,
    name: &'static str,
    description: &'static str,
    tool: &'static str,
    kind: SkillKind,
    provider: Option<&'static ProviderReq>,
) -> SkillDef {
    SkillDef {
        id,
        name,
        description,
        tool: Some(tool),
        kind,
        provider,
    }
}

/// Terse constructor for a **tool-less** built-in row.
///
/// Why: The owner's model explicitly admits skills with no executable member
/// ("There can be other skills without tools"). Giving them their own
/// constructor makes them a declared shape rather than a `None` that reads like
/// an oversight.
/// What: Builds a [`SkillDef`] with `tool: None`.
/// Test: `tool_less_skills_are_present_and_expand_to_no_tools`.
pub const fn prose_skill(
    id: &'static str,
    name: &'static str,
    description: &'static str,
    kind: SkillKind,
) -> SkillDef {
    SkillDef {
        id,
        name,
        description,
        tool: None,
        kind,
        provider: None,
    }
}

/// Where a manifest came from.
///
/// Why: DOC-57 S-10's badge requirement, restated for the 1:1 model — an
/// unwrapped surface must be *visible*, never indistinguishable from curated
/// capability. `Derived` is the honest label for a runtime-discovered MCP tool
/// nobody has named yet.
/// What: `Builtin` (this crate's `const` catalog), `Authored` (a `.md`
/// frontmatter manifest from a skill source, carrying its source path),
/// `Derived` (synthesised 1:1 from a tool name at request time).
/// Test: `authored_manifest_overrides_builtin_by_tool`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SkillOrigin {
    Builtin,
    Authored { path: String },
    Derived,
}

/// One resolved skill: a human-named capability wrapping zero or one tool.
///
/// Why: The unit the user reasons about and the unit `[skills].allow` names. A
/// raw tool identifier is an implementation detail beneath it — which is why
/// `name` is prose ("MTA Train Time") and never an echo of `tool`.
/// What: Owned form of [`SkillDef`], plus provenance. `tools` is a `Vec` rather
/// than an `Option<String>` so a tool-less skill and a tool-wrapping skill share
/// one shape and the API response needs no special case; the 1:1 invariant caps
/// its length at one and is asserted, not assumed.
/// Test: `skills/manifest/tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: SkillKind,
    pub tools: Vec<String>,
    pub provider: Option<ProviderReq>,
    pub origin: SkillOrigin,
}

impl SkillManifest {
    /// Adopt a compile-time row into an owned manifest.
    ///
    /// Why/What: Single conversion point so `builtin` tables stay `const`.
    /// Test: `builtin_rows_claim_each_tool_once`.
    fn from_def(def: &SkillDef) -> Self {
        Self {
            id: def.id.to_string(),
            name: def.name.to_string(),
            description: def.description.to_string(),
            kind: def.kind,
            tools: def.tool.map(str::to_string).into_iter().collect(),
            provider: def.provider.copied(),
            origin: SkillOrigin::Builtin,
        }
    }

    /// Synthesise a 1:1 manifest for a tool no authored/built-in row claims.
    ///
    /// Why: DOC-57 C-04.3 — every tool in the effective set maps to exactly one
    /// skill, so the pane has no gaps. Live-discovered MCP tools
    /// (`mcp_live`, `mcp_service_tools`, OpenRPC endpoints) are not knowable at
    /// compile time, so the alternative to deriving is a hole in the pane.
    /// What: `name` is the tool identifier prettified into title case — and the
    /// manifest is marked `Derived` so the GUI can badge it. It deliberately
    /// does NOT invent a description or a provider: an empty description that
    /// the pane renders as "no manifest authored" is honest; a plausible
    /// sentence nobody wrote is the fabrication #3945 forbids.
    /// Test: `derived_manifest_is_marked_and_carries_no_invented_prose`.
    pub fn derived(tool: &str) -> Self {
        Self {
            id: format!("derived:{tool}"),
            name: humanize_tool_name(tool),
            description: String::new(),
            kind: SkillKind::Action,
            tools: vec![tool.to_string()],
            provider: None,
            origin: SkillOrigin::Derived,
        }
    }

    /// The single tool this skill wraps, if any.
    ///
    /// Why/What: Expresses the 1:1 rule at the read sites instead of making
    /// each of them index `tools[0]` and hope.
    /// Test: `tool_less_skills_are_present_and_expand_to_no_tools`.
    pub fn tool(&self) -> Option<&str> {
        self.tools.first().map(String::as_str)
    }
}

/// Title-case a snake_case tool identifier for a derived skill's display name.
///
/// Why: `get_train_schedule` is not a name a user reads; "Get Train Schedule"
/// at least parses as English. This is a *fallback* for tools nobody has named
/// — every built-in row supplies a real human name instead.
/// What: Splits on `_`, upper-cases each initial. Empty input returns empty.
/// Test: `humanize_tool_name_title_cases_segments`.
fn humanize_tool_name(tool: &str) -> String {
    tool.split('_')
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The resolved skill set: built-in rows overridden by authored manifests.
///
/// Why: Authored always beats built-in (DOC-57 S-9's precedence, restated for
/// 1:1) so an operator can rename or re-describe any capability by dropping a
/// `.md` into a skill source — without recompiling, and without the catalog and
/// the prose drifting into two answers.
/// What: Insertion-ordered manifests plus a tool→index map. Construction is
/// total: a duplicate id replaces, and a second manifest claiming an
/// already-claimed tool is REJECTED (the 1:1 invariant is enforced at build
/// time, not asserted downstream).
/// Test: `authored_manifest_overrides_builtin_by_tool`, `builtin_rows_claim_each_tool_once`.
#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    manifests: Vec<SkillManifest>,
    by_tool: HashMap<String, usize>,
}

impl SkillCatalog {
    /// The compile-time catalog shipped with this crate.
    ///
    /// Why/What: Every tool trusty-agents registers in-process, plus the
    /// first-party `trusty-gworkspace` MCP surface and the platform tools the
    /// checked-in agent roster grants, each with a human name.
    /// Test: `every_tool_declared_in_source_has_a_skill`.
    pub fn builtin() -> Self {
        let mut catalog = Self::default();
        for def in builtin::all() {
            catalog.insert(SkillManifest::from_def(def));
        }
        catalog
    }

    /// Overlay authored manifests on top of an existing catalog.
    ///
    /// Why: Authored beats built-in, and an authored manifest that re-describes
    /// an already-wrapped tool must REPLACE the built-in row rather than create
    /// a second skill for the same tool (which would break 1:1).
    ///
    /// One field is INHERITED rather than replaced: `provider`. The `.md`
    /// frontmatter dialect has no key for a credential requirement, so an
    /// authored manifest always arrives with `provider: None` — and dropping the
    /// built-in's requirement would silently turn "Brave Search API key needed"
    /// into a card that implies none is. Losing a credential warning during a
    /// migration is exactly the kind of quiet honesty regression #3945 exists to
    /// prevent, so the replaced row's provider carries over.
    /// What: For each authored manifest, inherits `provider` from the row it
    /// displaces when it declares none, drops any manifest claiming the same
    /// tool or id, then inserts.
    /// Test: `authored_manifest_overrides_builtin_by_tool`,
    /// `authored_manifest_inherits_the_builtin_provider_requirement`.
    pub fn with_authored(mut self, authored: Vec<SkillManifest>) -> Self {
        for mut manifest in authored {
            if manifest.provider.is_none()
                && let Some(tool) = manifest.tool()
                && let Some(existing) = self.skill_for_tool(tool)
            {
                manifest.provider = existing.provider;
            }
            self.remove_conflicts(&manifest);
            self.insert(manifest);
        }
        self
    }

    /// Drop any manifest sharing this one's id or its wrapped tool.
    fn remove_conflicts(&mut self, incoming: &SkillManifest) {
        self.manifests
            .retain(|m| m.id != incoming.id && (m.tool().is_none() || m.tool() != incoming.tool()));
        self.reindex();
    }

    /// Append a manifest unless its tool is already claimed.
    ///
    /// Why: Enforces the 1:1 invariant structurally. A built-in table typo that
    /// double-claims a tool loses the second row here AND fails
    /// `builtin_rows_claim_each_tool_once`, rather than silently producing two
    /// cards for one capability.
    /// What: Skips insertion when `tool` is already indexed; tool-less skills
    /// always insert.
    fn insert(&mut self, manifest: SkillManifest) {
        if let Some(tool) = manifest.tool()
            && self.by_tool.contains_key(tool)
        {
            return;
        }
        if let Some(tool) = manifest.tool() {
            self.by_tool.insert(tool.to_string(), self.manifests.len());
        }
        self.manifests.push(manifest);
    }

    fn reindex(&mut self) {
        self.by_tool.clear();
        for (idx, manifest) in self.manifests.iter().enumerate() {
            if let Some(tool) = manifest.tool() {
                self.by_tool.insert(tool.to_string(), idx);
            }
        }
    }

    /// Every manifest, in catalog order.
    pub fn manifests(&self) -> &[SkillManifest] {
        &self.manifests
    }

    /// Look a skill up by its stable id.
    pub fn get(&self, id: &str) -> Option<&SkillManifest> {
        self.manifests.iter().find(|m| m.id == id)
    }

    /// The single skill wrapping `tool`, if one is authored or built in.
    pub fn skill_for_tool(&self, tool: &str) -> Option<&SkillManifest> {
        self.by_tool.get(tool).map(|&idx| &self.manifests[idx])
    }

    /// Expand `[skills].allow` ids into the exact tool names they wrap.
    ///
    /// Why: **This is the narrow-never-widen property**, and it is the reason
    /// expansion is a pure function with its own test. An unknown id contributes
    /// NOTHING and is recorded in `unresolved` — it never falls back to "all
    /// tools", never returns `None` (which downstream means *unrestricted*), and
    /// never widens the set. A resolution bug can therefore only cost an agent
    /// capability it declared, never hand it capability it did not.
    /// What: Returns exact tool names (deduped, catalog order) plus the ids that
    /// resolved to nothing. Tool-less skills resolve successfully and contribute
    /// zero tools — they are not "unresolved".
    /// Test: `unknown_skill_id_yields_no_tools_never_all_tools`,
    /// `unknown_skill_id_yields_no_tools_never_all_tools`, `tool_less_skills_are_present_and_expand_to_no_tools`.
    pub fn expand(&self, allow: &[String]) -> SkillExpansion {
        let mut tools: Vec<String> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut unresolved: Vec<String> = Vec::new();
        for id in allow {
            match self.get(id) {
                Some(manifest) => {
                    if let Some(tool) = manifest.tool()
                        && seen.insert(tool)
                    {
                        tools.push(tool.to_string());
                    }
                }
                None => unresolved.push(id.clone()),
            }
        }
        SkillExpansion { tools, unresolved }
    }
}

/// The outcome of expanding a `[skills].allow` list.
///
/// Why: A dangling skill reference is surfaced, never silently dropped
/// (DOC-57 S-11) — so expansion returns the misses alongside the hits.
/// What: `tools` are exact names; `unresolved` are ids no manifest matched.
/// Test: `unknown_skill_id_yields_no_tools_never_all_tools`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillExpansion {
    pub tools: Vec<String>,
    pub unresolved: Vec<String>,
}

/// Compile `[skills].allow` + `[tools].allow` down to one allow-pattern list.
///
/// Why: **The whole compatibility story lives here.** The result is handed
/// straight to the existing `filter_persona_tool_names` gate, so the RBAC tier
/// filter, the scope gate and `persona_allowed_tools`'s deliberate never-`None`
/// behaviour are all untouched. Union rather than replace (DOC-57 §9.3) is
/// deliberate: replacement would mean adding one `[skills]` line silently
/// REMOVES every capability the agent had from `[tools].allow`, and because
/// agent files are local and unversioned that surfaces as an agent mysteriously
/// losing abilities.
///
/// **Precedence:** `[tools].allow` entries come first and are preserved
/// verbatim — globs included — then skill-expanded exact names are appended if
/// not already present. Neither section can remove what the other grants.
///
/// What: Returns `None` only when BOTH sections are absent, which is byte-
/// identical to today's behaviour (the caller's `if let Some(patterns)` arm is
/// what decides a persona gets zero tools). `Some(vec![])` — from a present but
/// empty section — keeps denying every call, unchanged.
/// Test: `no_skills_section_is_byte_identical_to_tools_allow`,
/// `skills_only_agent_gets_exactly_the_manifest_tools`,
/// `union_preserves_tools_allow_globs`, `absent_sections_yield_none`.
pub fn effective_tool_patterns(
    tools_allow: Option<&Vec<String>>,
    skills_allow: Option<&Vec<String>>,
    catalog: &SkillCatalog,
) -> (Option<Vec<String>>, Vec<String>) {
    match (tools_allow, skills_allow) {
        (None, None) => (None, Vec::new()),
        (Some(tools), None) => (Some(tools.clone()), Vec::new()),
        (tools, Some(skills)) => {
            let expansion = catalog.expand(skills);
            let mut patterns = tools.cloned().unwrap_or_default();
            for tool in expansion.tools {
                if !patterns.contains(&tool) {
                    patterns.push(tool);
                }
            }
            (Some(patterns), expansion.unresolved)
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
