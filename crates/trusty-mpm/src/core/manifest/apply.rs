//! Translate a resolved [`HarnessManifest`] into concrete provisioning inputs.
//!
//! Why: `prepare_session` consumes the resolved manifest, but it should not have
//! to know how each manifest section maps onto a source directory, a selection
//! predicate, or an MCP toggle. Centralizing that translation here keeps
//! `session_launch/mod.rs` focused on the provisioning *sequence* and keeps the
//! manifest→deployment mapping independently testable.
//! What: [`HarnessPlan`] is the materialized provisioning plan derived from a
//! manifest plus the framework/catalog roots — the agent and skill source dirs
//! (bundled vs catalog), the agent/skill selection predicates, the MCP toggles,
//! and the manifest's default output-style id. [`HarnessPlan::from_manifest`]
//! builds it.
//! Test: `plan_default_uses_bundled_sources`, `plan_catalog_source_paths`,
//! `plan_selection_filters`, `plan_mcp_toggles`.

use std::path::PathBuf;

use super::schema::{ContentSource, HarnessManifest, selection_matches};
use crate::core::paths::FrameworkPaths;

/// A materialized provisioning plan derived from a resolved manifest.
///
/// Why: `prepare_session` needs concrete inputs (where to read agents/skills
/// from, which to deploy, which MCP servers to inject, the default style) rather
/// than a manifest it must re-interpret inline; bundling them keeps the launch
/// path readable.
/// What: source directories already resolved to bundled-or-catalog paths, the
/// include/exclude lists for the agent and skill selection predicates, the two
/// MCP toggles (defaulting to on), and the optional manifest-default style id.
/// Test: `plan_default_uses_bundled_sources`, `plan_catalog_source_paths`.
#[derive(Debug, Clone)]
pub struct HarnessPlan {
    /// Directory the agent *source* files are read from.
    pub agent_source: PathBuf,
    /// Agent include patterns (empty = all).
    pub agent_include: Vec<String>,
    /// Agent exclude patterns.
    pub agent_exclude: Vec<String>,
    /// Agent name/glob patterns exempt from catalog-staleness reporting only
    /// (issue #2462). Does NOT affect [`Self::agent_selected`] — an agent
    /// listed here still deploys normally; this only silences the drift
    /// warning for agents the operator has intentionally customized. See
    /// [`super::schema::AgentSet::ignore_staleness`] for the full rationale.
    pub agent_ignore_staleness: Vec<String>,
    /// Directory the skill *source* files are read from.
    pub skill_source: PathBuf,
    /// Skill include patterns (empty = all).
    pub skill_include: Vec<String>,
    /// Skill exclude patterns.
    pub skill_exclude: Vec<String>,
    /// Whether to inject the `trusty-memory` MCP server.
    pub inject_trusty_memory: bool,
    /// Whether to inject the `trusty-search` MCP server.
    pub inject_trusty_search: bool,
    /// The manifest's default output-style id, if it sets one.
    ///
    /// This is the LOWEST-precedence style input: an explicit `--style` flag and
    /// the `[style] active` config key both override it (HR-4 precedence is
    /// preserved by the launch path).
    pub style: Option<String>,
}

impl HarnessPlan {
    /// Build a provisioning plan from a resolved manifest.
    ///
    /// Why: this is the single place the manifest's declarative sections become
    /// concrete provisioning inputs, so the manifest→deployment mapping can be
    /// audited and tested in one spot.
    /// What: resolves each content source to a directory — `Bundled` uses the
    /// framework's own `agent_source_dir()`/`skill_source_dir()`; `Catalog` uses
    /// `<catalog_root>/repo/.claude/agents` and `…/skills` (the layout
    /// `CatalogSync` writes). Copies the include/exclude lists, reads the MCP
    /// toggles (absent → on, matching the default), and extracts the style id.
    /// Absent sections fall back to bundled-source/all/on so a default manifest
    /// reproduces today's behavior.
    /// Test: `plan_default_uses_bundled_sources`, `plan_catalog_source_paths`,
    /// `plan_mcp_toggles`.
    pub fn from_manifest(
        manifest: &HarnessManifest,
        fw: &FrameworkPaths,
        catalog_root: &std::path::Path,
    ) -> Self {
        let catalog_agents = catalog_root.join("repo").join(".claude").join("agents");
        let catalog_skills = catalog_root.join("repo").join(".claude").join("skills");

        let (agent_source, agent_include, agent_exclude, agent_ignore_staleness) =
            match &manifest.agents {
                Some(set) => {
                    let source = match set.source {
                        ContentSource::Bundled => fw.agent_source_dir(),
                        ContentSource::Catalog => catalog_agents,
                    };
                    (
                        source,
                        set.include.clone(),
                        set.exclude.clone(),
                        set.ignore_staleness.clone(),
                    )
                }
                None => (fw.agent_source_dir(), Vec::new(), Vec::new(), Vec::new()),
            };

        let (skill_source, skill_include, skill_exclude) = match &manifest.skills {
            Some(set) => {
                let source = match set.source {
                    ContentSource::Bundled => fw.skill_source_dir(),
                    ContentSource::Catalog => catalog_skills,
                };
                (source, set.include.clone(), set.exclude.clone())
            }
            None => (fw.skill_source_dir(), Vec::new(), Vec::new()),
        };

        // MCP toggles: absent section or absent flag → on (today's behavior).
        let (inject_trusty_memory, inject_trusty_search) = match &manifest.mcp {
            Some(mcp) => (
                mcp.trusty_memory.unwrap_or(true),
                mcp.trusty_search.unwrap_or(true),
            ),
            None => (true, true),
        };

        let style = manifest.style.as_ref().and_then(|s| s.active.clone());

        Self {
            agent_source,
            agent_include,
            agent_exclude,
            agent_ignore_staleness,
            skill_source,
            skill_include,
            skill_exclude,
            inject_trusty_memory,
            inject_trusty_search,
            style,
        }
    }

    /// Whether an agent stem is selected by this plan.
    ///
    /// Why: the agent deploy step takes a predicate; exposing the manifest's
    /// include/exclude rule as a method keeps the rule next to the plan it came
    /// from.
    /// What: delegates to [`selection_matches`] with the plan's agent lists.
    /// Test: `plan_selection_filters`.
    pub fn agent_selected(&self, stem: &str) -> bool {
        selection_matches(stem, &self.agent_include, &self.agent_exclude)
    }

    /// Whether an agent stem is exempt from catalog-staleness reporting.
    ///
    /// Why (issue #2462): `detect_staleness` must be able to silence the drift
    /// warning for an agent the operator deliberately customized WITHOUT that
    /// exemption also removing the agent from [`Self::agent_selected`]'s deploy
    /// roster — the two concerns are independent (see
    /// [`super::schema::AgentSet::ignore_staleness`]).
    /// What: `true` iff `stem` matches any `agent_ignore_staleness` pattern.
    /// Test: `plan_ignore_staleness_does_not_affect_agent_selected`.
    pub fn agent_staleness_ignored(&self, stem: &str) -> bool {
        super::schema::matches_any(stem, &self.agent_ignore_staleness)
    }

    /// Whether a skill stem is selected by this plan.
    ///
    /// Why/What/Test: as [`agent_selected`](Self::agent_selected), for skills.
    pub fn skill_selected(&self, stem: &str) -> bool {
        selection_matches(stem, &self.skill_include, &self.skill_exclude)
    }
}

#[cfg(test)]
mod tests {
    use super::super::default::default_manifest;
    use super::super::schema::{AgentSet, McpServers, SkillSet, StyleSelection};
    use super::*;

    #[test]
    fn plan_default_uses_bundled_sources() {
        // The compiled-in default manifest must plan bundled sources, no filter,
        // both MCP servers on, and the professional style — i.e. today's path.
        let fw = FrameworkPaths::under("/base");
        let plan = HarnessPlan::from_manifest(
            &default_manifest(),
            &fw,
            std::path::Path::new("/home/.trusty-mpm/catalog"),
        );
        assert_eq!(plan.agent_source, fw.agent_source_dir());
        assert_eq!(plan.skill_source, fw.skill_source_dir());
        assert!(plan.agent_include.is_empty());
        assert!(plan.agent_selected("anything"));
        assert!(plan.skill_selected("anything"));
        assert!(plan.inject_trusty_memory);
        assert!(plan.inject_trusty_search);
        assert_eq!(plan.style.as_deref(), Some("trusty-mpm"));
    }

    #[test]
    fn plan_catalog_source_paths() {
        // A manifest pointing agents at the catalog must resolve to the
        // <catalog>/repo/.claude/agents layout CatalogSync writes.
        let fw = FrameworkPaths::under("/base");
        let manifest = HarnessManifest {
            agents: Some(AgentSet {
                include: vec![],
                exclude: vec![],
                source: ContentSource::Catalog,
                ..AgentSet::default()
            }),
            ..default_manifest()
        };
        let plan = HarnessPlan::from_manifest(
            &manifest,
            &fw,
            std::path::Path::new("/home/.trusty-mpm/catalog"),
        );
        assert_eq!(
            plan.agent_source,
            PathBuf::from("/home/.trusty-mpm/catalog/repo/.claude/agents")
        );
        // skills stayed bundled (default manifest's skills section).
        assert_eq!(plan.skill_source, fw.skill_source_dir());
    }

    #[test]
    fn plan_selection_filters() {
        // include/exclude on the manifest must drive the selection predicates.
        let fw = FrameworkPaths::under("/base");
        let manifest = HarnessManifest {
            agents: Some(AgentSet {
                include: vec!["*-engineer".into()],
                exclude: vec!["php-engineer".into()],
                source: ContentSource::Bundled,
                ..AgentSet::default()
            }),
            skills: Some(SkillSet {
                include: vec!["example-skill".into()],
                exclude: vec![],
                source: ContentSource::Bundled,
            }),
            ..default_manifest()
        };
        let plan = HarnessPlan::from_manifest(&manifest, &fw, std::path::Path::new("/c"));
        assert!(plan.agent_selected("rust-engineer"));
        assert!(!plan.agent_selected("php-engineer"), "exclude wins");
        assert!(!plan.agent_selected("qa"), "not in include");
        assert!(plan.skill_selected("example-skill"));
        assert!(!plan.skill_selected("tm-doctor"));
    }

    #[test]
    fn plan_ignore_staleness_does_not_affect_agent_selected() {
        // Issue #2462: `ignore_staleness` must silence the staleness-only
        // predicate while leaving `agent_selected` (the deploy roster) fully
        // unaffected — the exact confusion that let `exclude = ["engineer"]`
        // (meant only to silence /health) silently drop `engineer.md` from
        // every session-local `.claude/agents/`.
        let fw = FrameworkPaths::under("/base");
        let manifest = HarnessManifest {
            agents: Some(AgentSet {
                include: vec![],
                exclude: vec![],
                ignore_staleness: vec!["engineer".into()],
                source: ContentSource::Bundled,
            }),
            ..default_manifest()
        };
        let plan = HarnessPlan::from_manifest(&manifest, &fw, std::path::Path::new("/c"));
        assert!(
            plan.agent_selected("engineer"),
            "ignore_staleness must not remove an agent from the deploy roster"
        );
        assert!(plan.agent_staleness_ignored("engineer"));
        assert!(
            !plan.agent_staleness_ignored("qa"),
            "unrelated agent unaffected"
        );
    }

    #[test]
    fn plan_mcp_toggles() {
        // Disabling an MCP server in the manifest must turn off injection.
        let fw = FrameworkPaths::under("/base");
        let manifest = HarnessManifest {
            mcp: Some(McpServers {
                trusty_memory: Some(false),
                trusty_search: Some(true),
            }),
            ..default_manifest()
        };
        let plan = HarnessPlan::from_manifest(&manifest, &fw, std::path::Path::new("/c"));
        assert!(!plan.inject_trusty_memory);
        assert!(plan.inject_trusty_search);
    }

    #[test]
    fn plan_style_default_lowest_precedence() {
        // The manifest style is exposed as the plan's style; an absent style
        // section yields None.
        let fw = FrameworkPaths::under("/base");
        let manifest = HarnessManifest {
            style: Some(StyleSelection {
                active: Some("trusty-mpm-research".into()),
            }),
            ..HarnessManifest::default()
        };
        let plan = HarnessPlan::from_manifest(&manifest, &fw, std::path::Path::new("/c"));
        assert_eq!(plan.style.as_deref(), Some("trusty-mpm-research"));

        let none_plan = HarnessPlan::from_manifest(
            &HarnessManifest::default(),
            &fw,
            std::path::Path::new("/c"),
        );
        assert_eq!(none_plan.style, None);
    }
}
