//! Skill manifests: the one-skill-per-tool wrapping layer (#3933, DOC-57 §5).
//!
//! Why: Owner directive, 2026-07-25 — *"One skill per tool. There can be other
//! skills without tools, but each \[tool\] needs an accompanying skill"*, refining
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
/// (Skills pane, collapsed under "System"), `Function` (#4022 — a *bundling*
/// row, rendered as a group header over its member cards rather than as a
/// capability of its own).
/// Test: `knowledge_kind_routes_out_of_the_skills_pane`,
/// `builtin_function_rows_declare_members_and_no_tool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillKind {
    Action,
    Knowledge,
    System,
    /// #4022: a function skill — names member skill ids, wraps no tool itself.
    Function,
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
    /// #4022: member skill ids, for a **function skill** only.
    ///
    /// Why: Mutually exclusive with `tool` — a row is either a leaf wrapping
    /// exactly one tool, or a bundle naming N leaves, never both. Modelling the
    /// empty case as an empty slice rather than an `Option` keeps every existing
    /// `const fn` constructor total and lets read sites ask `is_function()`
    /// instead of pattern-matching two shapes.
    /// Test: `builtin_function_rows_declare_members_and_no_tool`.
    pub members: &'static [&'static str],
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
        members: &[],
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
        members: &[],
    }
}

/// Terse constructor for a **function skill** — a bundle of member skill ids.
///
/// Why (#4022, epic #4021 OQ-1 resolved *grant-the-bundle*): the owner's model
/// is *"'ticketing' is a skill with various capabilities including all the
/// skills under one"*. One `[skills].allow` entry should grant a whole
/// capability. This is deliberately a **manifest-layer** primitive, not a new
/// matcher: a function skill still resolves to an exact, closed, compile-time
/// set of leaf ids, so it COMPILES DOWN to the same 1:1 rows the permission
/// gates already see. No glob dialect is added, `is_exact_tool_name` still
/// guards every name that reaches an allow-pattern, and the bundle id itself
/// never leaves this layer.
/// What: Builds a [`SkillDef`] with `tool: None`, `kind: Function`, and the
/// given member ids. Members are leaf (or nested function) skill ids resolved
/// against the same catalog — unknown ids are a **manifest validation error**
/// caught at build/test time by [`SkillCatalog::unknown_function_members`], and
/// at runtime they narrow (contribute zero tools, surface in `unresolved`).
/// Test: `function_skill_expands_to_the_union_of_its_members_tools`,
/// `function_skill_never_leaks_its_bundle_id_into_the_tool_patterns`.
pub const fn function_skill(
    id: &'static str,
    name: &'static str,
    description: &'static str,
    members: &'static [&'static str],
) -> SkillDef {
    SkillDef {
        id,
        name,
        description,
        tool: None,
        kind: SkillKind::Function,
        provider: None,
        members,
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
    /// #4022: member skill ids when this is a function skill; empty otherwise.
    ///
    /// Why: Authored `.md` frontmatter has no `members:` key and its parser
    /// requires a `tools:` list, so an AUTHORED manifest is always a leaf and
    /// this is always empty for it. Bundles are therefore built-in-only in this
    /// slice — an operator cannot drop a `.md` into a skill source that widens
    /// one grant into N. That is a deliberate narrowing of the attack surface,
    /// not an oversight.
    /// Test: `authored_manifest_cannot_declare_a_bundle`.
    pub members: Vec<String>,
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
            // #4022: bundles are declared in the const table, adopted verbatim.
            members: def.members.iter().map(|m| (*m).to_string()).collect(),
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
            members: Vec::new(),
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

    /// Whether this row is a #4022 bundle rather than a leaf capability.
    ///
    /// Why: Read sites must distinguish "tool-less because it is prose guidance"
    /// from "tool-less because it is a group header" WITHOUT inspecting
    /// `members` — a bundle whose members all failed to resolve is still a
    /// bundle, and treating it as a tool-less prose skill would grant it by id.
    /// What: True when `kind` is [`SkillKind::Function`].
    /// Test: `granted_ids_do_not_grant_a_function_skill_by_id_alone`.
    pub fn is_function(&self) -> bool {
        self.kind == SkillKind::Function
    }
}

/// Characters that make a string a PATTERN rather than an exact tool name.
///
/// Why: `match_any_glob` (`ctrl/pm_task/helpers.rs`) treats a trailing `*` as a
/// prefix wildcard, so `*` alone matches every tool in the registry. `?` and
/// `[`/`]` are not part of that dialect today, but they are glob metacharacters
/// in every dialect a future author would reach for, and rejecting them now
/// costs nothing while leaving no room for the next matcher change to reopen
/// this hole.
const GLOB_METACHARACTERS: &[char] = &['*', '?', '[', ']'];

/// Whether `name` is an exact tool name safe to expand into an allow-pattern.
///
/// Why — **this is a privilege-escalation gate, not tidiness.** Skill-expanded
/// names are merged into the very list handed to `match_any_glob`. An authored
/// `.md` carrying `tools: ["*"]` would therefore turn one innocuous
/// `[skills].allow` entry into `[tools].allow = ["*"]` — every tool in the
/// registry — behind a skill card that renders as a single narrow capability.
/// That is strictly WORSE than the raw glob it replaces, because the reviewer
/// reading `agent.toml` sees a tidy skill id and no wildcard at all. DOC-57 S-1
/// already says `tools` entries are exact names and "the skill layer adds no
/// fourth glob dialect"; before this check, nothing enforced it.
/// What: `true` only for a non-empty name with no leading/trailing whitespace
/// and no [`GLOB_METACHARACTERS`].
/// Test: `authored_glob_shaped_tool_entry_is_rejected_not_granted`,
/// `is_exact_tool_name_rejects_every_glob_metacharacter`.
pub fn is_exact_tool_name(name: &str) -> bool {
    !name.is_empty() && name.trim() == name && !name.contains(GLOB_METACHARACTERS)
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
        // #4022: a bundle naming a non-existent member is a manifest bug, and it
        // is caught here in every debug build plus
        // `builtin_function_skills_declare_only_known_members` in CI. Release
        // builds fall through to expansion's narrow-never-widen behaviour rather
        // than aborting a running daemon over a catalog typo.
        #[cfg(debug_assertions)]
        {
            let unknown = catalog.unknown_function_members();
            debug_assert!(
                unknown.is_empty(),
                "built-in function skills name unknown members: {unknown:?}"
            );
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
    /// **Source precedence:** `skills/sources/mod.rs` scans higher-priority
    /// sources first and documents "first writer wins", and `load_from_paths`
    /// preserves that order. So among AUTHORED manifests the first one to claim
    /// a tool keeps it and later (lower-priority) ones are skipped. Authored
    /// still displaces BUILT-IN unconditionally — that is a different axis, and
    /// collapsing the two would either let a low-priority source override a
    /// high-priority one or stop authored manifests overriding the catalog at
    /// all.
    /// What: For each authored manifest, inherits `provider` from the row it
    /// displaces when it declares none, drops any BUILT-IN manifest claiming the
    /// same tool or id, then inserts (a conflicting authored row wins by being
    /// there first).
    /// Test: `authored_manifest_overrides_builtin_by_tool`,
    /// `authored_manifest_inherits_the_builtin_provider_requirement`,
    /// `authored_multi_source_precedence_first_source_wins`.
    pub fn with_authored(mut self, authored: Vec<SkillManifest>) -> Self {
        for mut manifest in authored {
            if manifest.provider.is_none()
                && let Some(tool) = manifest.tool()
                && let Some(existing) = self.skill_for_tool(tool)
            {
                manifest.provider = existing.provider;
            }
            self.remove_displaced_builtins(&manifest);
            self.insert(manifest);
        }
        self
    }

    /// Drop any BUILT-IN manifest sharing this one's id or its wrapped tool.
    ///
    /// Why: Authored beats built-in (S-9), but an already-inserted AUTHORED
    /// manifest came from a higher-priority source and must survive — evicting
    /// it here is what silently inverted source precedence. Filtering on
    /// `origin` keeps both rules intact in one pass.
    ///
    /// **#4022 — a built-in FUNCTION row is exempt from id displacement.**
    /// Authored-beats-built-in is a *renaming* rule: it lets an operator
    /// re-describe a capability. Applied to a bundle id it stops being a rename
    /// and becomes a HIJACK — dropping `ticketing.md` with
    /// `tools: ["execute_shell_command"]` into any skill source would evict the
    /// ten-member bundle and leave `[skills].allow = ["ticketing"]` resolving to
    /// a shell, with no `unresolved` entry and nothing in `agent.toml` for a
    /// reviewer to notice. The bundle id is the one name whose meaning a
    /// reviewer cannot check by reading the config, so it is the one name a
    /// skill source may not redefine. An authored row may still displace a
    /// bundle's MEMBERS by id or tool — that path narrows and reports (see
    /// `authored_displacement_of_a_member_narrows_the_bundle_and_reports_it`).
    /// What: Retains everything except built-in rows colliding by id or tool,
    /// EXCEPT built-in function rows, which survive an id collision and log it.
    /// Test: `authored_multi_source_precedence_first_source_wins`,
    /// `authored_md_cannot_displace_a_builtin_bundle_id`.
    fn remove_displaced_builtins(&mut self, incoming: &SkillManifest) {
        self.manifests.retain(|m| {
            if m.origin != SkillOrigin::Builtin {
                return true;
            }
            if m.is_function() && m.id == incoming.id {
                tracing::warn!(
                    skill = %m.id,
                    "an authored skill manifest claims a built-in function-skill id; \
                     refusing to displace the bundle (a bundle id may not be redefined \
                     by a skill source)"
                );
                return true;
            }
            m.id != incoming.id && (m.tool().is_none() || m.tool() != incoming.tool())
        });
        self.reindex();
    }

    /// Append a manifest unless its tool is already claimed or glob-shaped.
    ///
    /// Why: Two invariants enforced structurally rather than trusted.
    /// **1:1** — a built-in table typo that double-claims a tool loses the second
    /// row here AND fails `builtin_rows_claim_each_tool_once`, rather than
    /// silently producing two cards for one capability. **Exact names** — this is
    /// the last line of defence for [`is_exact_tool_name`]: even if a future
    /// caller builds a `SkillManifest` without going through `authored::parse`,
    /// a glob-shaped tool never reaches `by_tool`, so it can never be expanded
    /// into an allow-pattern. Defence in depth on a privilege-escalation path is
    /// worth the duplicated check.
    /// **#4022 — a bundle id is never shadowed.** `remove_displaced_builtins`
    /// already refuses to evict a function row, but that leaves the incoming row
    /// free to sit behind it as a duplicate id — and `get` returning the first
    /// match makes "which one wins" a property of insertion order rather than of
    /// a stated rule. Refusing the insert makes the bundle the single answer for
    /// its id on every construction path, including hand-rolled ones that never
    /// go through `with_authored`. Defence in depth, same posture as the
    /// duplicated glob check above.
    /// What: Skips insertion when `tool` is already indexed, is not an exact
    /// name, or the id is already held by a function skill; tool-less skills
    /// otherwise always insert.
    /// Test: `authored_glob_shaped_tool_entry_is_rejected_not_granted`,
    /// `catalog_insert_refuses_a_glob_shaped_tool`,
    /// `authored_md_cannot_displace_a_builtin_bundle_id`.
    fn insert(&mut self, manifest: SkillManifest) {
        if self
            .manifests
            .iter()
            .any(|m| m.is_function() && m.id == manifest.id)
        {
            tracing::warn!(
                skill = %manifest.id,
                "a function skill already owns this id; refusing to shadow it"
            );
            return;
        }
        if let Some(tool) = manifest.tool()
            && (self.by_tool.contains_key(tool) || !is_exact_tool_name(tool))
        {
            if !is_exact_tool_name(tool) {
                tracing::warn!(
                    skill = %manifest.id,
                    tool = %tool,
                    "skill manifest names a glob-shaped tool; refusing to bind it"
                );
            }
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
    /// It is also the third and last place [`is_exact_tool_name`] is enforced.
    /// Expansion output is merged straight into the patterns `match_any_glob`
    /// consumes, so this function is the final gate before a name acquires
    /// wildcard authority. Re-checking a value that `insert` already refused is
    /// deliberate: it costs one comparison and it means no future construction
    /// path can smuggle a wildcard through.
    ///
    /// **#4022 — function skills compile down here and only here.** A bundle id
    /// is replaced by its members' tools *before* this function returns, so the
    /// 1:1 layer and all three permission gates downstream see nothing but leaf
    /// tool names. The bundle id is never itself a pattern.
    /// What: Returns exact tool names (deduped, catalog order) plus the ids that
    /// resolved to nothing. Tool-less skills resolve successfully and contribute
    /// zero tools — they are not "unresolved".
    /// Test: `unknown_skill_id_yields_no_tools_never_all_tools`,
    /// `authored_glob_shaped_tool_entry_is_rejected_not_granted`,
    /// `tool_less_skills_are_present_and_expand_to_no_tools`,
    /// `function_skill_never_leaks_its_bundle_id_into_the_tool_patterns`.
    pub fn expand(&self, allow: &[String]) -> SkillExpansion {
        let mut acc = Expansion::default();
        for id in allow {
            self.expand_id(id, &mut acc);
        }
        SkillExpansion {
            tools: acc.tools,
            unresolved: acc.unresolved,
        }
    }

    /// Resolve one skill id into `acc`, recursing through function members.
    ///
    /// Why (#4022): The bundling tier must not become a second resolution rule.
    /// Expansion stays one recursive walk that bottoms out in the SAME leaf
    /// check — `tool()` + [`is_exact_tool_name`] — so every property PR #3964
    /// pinned holds for a bundle by construction rather than by a parallel
    /// implementation that can drift: an unknown member contributes nothing and
    /// is REPORTED (never "all tools"), and a bundle can only ever reach tools
    /// its members already wrap.
    ///
    /// `acc.expanded` is a MEMOISATION set, and it is load-bearing for both
    /// correctness and cost. Expanding an id is idempotent — tools dedupe
    /// through `seen_tools` and a miss is recorded once — so a second visit can
    /// only redo work. Recording ids permanently (rather than unwinding a
    /// visited-set on the way out, which only bounds DEPTH) makes the walk
    /// linear in the catalog and cycle-safe by the same stroke. The distinction
    /// is not academic: an unwinding guard re-walks a DAG once per distinct
    /// path, so a 24-level fan-out-2 bundle graph costs ~16.7M recursions, and
    /// `expand` runs on EVERY persona turn and subagent launch — a catalog
    /// shape, not untrusted input, would have become a latent CPU stall.
    /// What: Already-expanded → return. Leaf → push its tool if new. Function →
    /// recurse over members. Unknown → record in `unresolved`.
    /// Test: `function_skill_member_cycle_terminates_without_widening`,
    /// `function_skill_fan_out_expands_in_linear_work`.
    fn expand_id(&self, id: &str, acc: &mut Expansion) {
        if !acc.expanded.insert(id.to_string()) {
            return;
        }
        let Some(manifest) = self.get(id) else {
            acc.unresolved.push(id.to_string());
            return;
        };
        if let Some(tool) = manifest.tool() {
            if is_exact_tool_name(tool) && acc.seen_tools.insert(tool.to_string()) {
                acc.tools.push(tool.to_string());
            }
            return;
        }
        if !manifest.is_function() {
            return; // Tool-less prose skill: resolves, contributes nothing.
        }
        for member in &manifest.members {
            self.expand_id(member, acc);
        }
    }

    /// Function-skill members no manifest in this catalog resolves.
    ///
    /// Why (#4022): A bundle naming a member that does not exist is a MANIFEST
    /// BUG, and the owner's instruction is that it fail at build/test time
    /// rather than degrade silently in production. Runtime expansion already
    /// narrows safely, so this is not a safety net — it is the tripwire that
    /// makes a catalog rename (a leaf skill id changed, its bundle not updated)
    /// fail CI instead of quietly shrinking an agent's capability.
    /// What: `(bundle_id, unknown_member_id)` pairs, in catalog order. Empty is
    /// the healthy state, and `SkillCatalog::builtin` `debug_assert!`s it.
    /// Test: `builtin_function_skills_declare_only_known_members`,
    /// `unknown_function_member_is_reported_by_validation`.
    pub fn unknown_function_members(&self) -> Vec<(String, String)> {
        self.manifests
            .iter()
            .flat_map(|m| {
                m.members
                    .iter()
                    .filter(|member| self.get(member).is_none())
                    .map(|member| (m.id.clone(), member.clone()))
            })
            .collect()
    }
}

/// Mutable accumulator threaded through [`SkillCatalog::expand_id`].
///
/// Why/What: Four parallel `&mut` parameters through a recursive walk is how
/// a resolution bug gets written; one struct keeps the dedup sets and the
/// output in lockstep. `expanded` is both the cycle guard and the memo table
/// (see [`SkillCatalog::expand_id`]); because it also makes each id reachable
/// once, `unresolved` needs no dedup scan of its own.
/// Test: `function_skill_expands_to_the_union_of_its_members_tools`,
/// `function_skill_fan_out_expands_in_linear_work`.
#[derive(Default)]
struct Expansion {
    tools: Vec<String>,
    seen_tools: BTreeSet<String>,
    unresolved: Vec<String>,
    expanded: BTreeSet<String>,
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
