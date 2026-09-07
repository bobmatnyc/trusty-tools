//! The compiled-in default harness manifest (HR-2 / DOC-17).
//!
//! Why: the NORMATIVE precedence (project override > user config > catalog >
//! compiled-in default) requires a fully-populated floor layer that, when no
//! higher layer overrides anything, reproduces *today's* provisioning behavior
//! exactly — so an absent manifest is a zero-regression no-op. This module is
//! that floor.
//! What: [`default_manifest`] returns a [`HarnessManifest`] with every section
//! set to the values that match the pre-HR-2 behavior: deploy ALL bundled agents
//! and skills, every instruction layer on, the professional output style, BOTH
//! MCP servers injected, and no per-tier model overrides (HR-1's built-in tier
//! mapping stands).
//! Test: `default_manifest_reproduces_bundled_behavior`,
//! `default_manifest_enables_both_mcp`, `default_manifest_carries_current_version`.

use super::schema::{
    AgentSet, ContentSource, DivertConfig, HarnessManifest, InstructionLayers, MANIFEST_VERSION,
    McpServers, ModelTiers, SkillSet, StyleSelection,
};

/// Build the compiled-in default harness manifest.
///
/// Why: this is the lowest-precedence layer; it must encode the exact behavior
/// `prepare_session` had before HR-2 so that, absent any override, provisioning
/// is unchanged (regression-safe).
/// What: returns a manifest that deploys every bundled agent/skill (empty
/// include = all), enables all instruction layers, selects the professional
/// `trusty-mpm` output style, injects both `trusty-memory` and `trusty-search`
/// MCP servers, leaves model tiers unset (HR-1 deploy-time mapping applies),
/// and states `[divert]` off (#6887 — the one section whose absent state is
/// off rather than on).
/// Test: `default_manifest_reproduces_bundled_behavior`,
/// `default_manifest_divert_disabled_by_default`.
pub fn default_manifest() -> HarnessManifest {
    HarnessManifest {
        version: Some(MANIFEST_VERSION),
        agents: Some(AgentSet {
            include: Vec::new(), // empty = all available agents
            exclude: Vec::new(),
            ignore_staleness: Vec::new(),
            source: ContentSource::Bundled,
        }),
        // The catalog declaration is the FRAMEWORK tier's business (#4760); the
        // compiled default is the floor every layer overlays, and it carries no
        // categories of its own. `ManifestSources::resolve` always applies the
        // framework tier's composed `[agents]` selection directly on top.
        agent_categories: None,
        // Likewise for the skill roster declaration (#4765).
        skill_categories: None,
        skills: Some(all_skills()),
        instructions: Some(InstructionLayers {
            system: Some(true),
            contextual: Some(true),
            domain: Some(true),
        }),
        style: Some(StyleSelection {
            active: Some(crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID.to_string()),
        }),
        mcp: Some(McpServers {
            trusty_memory: Some(true),
            trusty_search: Some(true),
            ..McpServers::default()
        }),
        models: Some(ModelTiers::default()),
        // #6887: OPT-IN — the floor layer states the feature off explicitly, so
        // a project that turns it on is the only way it ever runs.
        divert: Some(DivertConfig {
            enabled: Some(false),
            min_lines: Some(DEFAULT_DIVERT_MIN_LINES),
            worker_model: Some(DEFAULT_DIVERT_WORKER_MODEL.to_string()),
        }),
    }
}

/// Line count at or above which a bulk read is diverted (#6887).
///
/// Why: shunt's own default, and the value the #6882 POC measured against.
/// What: `350` — the floor layer's `[divert] min_lines`, and the fallback the
/// hook uses when `TRUSTY_DIVERT_MIN_LINES` is absent or unparseable.
/// Test: `default_manifest_divert_disabled_by_default`, `plan_divert_toggles`.
pub const DEFAULT_DIVERT_MIN_LINES: u32 = 350;

/// Default `[divert] worker_model` (#6887).
///
/// Why: Haiku 4.5 is the arm the #6882 POC measured. Owner ruling 2026-09-07
/// fixed the worker as headless Claude Code under the developer's existing
/// login, so this is a model name `claude --model` accepts, never a provider
/// route.
/// What: `"claude-haiku-4-5"`.
/// Test: `default_manifest_divert_disabled_by_default`, `plan_divert_toggles`.
pub const DEFAULT_DIVERT_WORKER_MODEL: &str = "claude-haiku-4-5";

/// The default skill set: deploy every bundled skill from the bundled source.
///
/// Why: keeps [`default_manifest`] readable by naming the all-skills default in
/// one place.
/// What: an empty-include (= all) [`SkillSet`] reading from the bundled source.
/// Test: covered by `default_manifest_reproduces_bundled_behavior`.
fn all_skills() -> SkillSet {
    SkillSet {
        include: Vec::new(),
        exclude: Vec::new(),
        source: ContentSource::Bundled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6887): bulk-read diversion changes what the agent may do, so the
    /// floor layer must state it OFF. A default that inherited the `[mcp]`
    /// "absent -> on" convention would turn every project's reads into worker
    /// calls the operator never asked for.
    /// What: asserts the compiled-in default's `[divert]` carries
    /// `enabled = Some(false)` and the documented `min_lines` / provider
    /// defaults.
    #[test]
    fn default_manifest_divert_disabled_by_default() {
        let divert = default_manifest().divert.expect("divert section present");
        assert_eq!(divert.enabled, Some(false), "divert must be opt-in");
        assert_eq!(divert.min_lines, Some(DEFAULT_DIVERT_MIN_LINES));
        assert_eq!(divert.min_lines, Some(350));
        assert_eq!(divert.worker_model.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn default_manifest_carries_current_version() {
        // The compiled-in default must stamp the current schema version so HR-3
        // hashing has a stable anchor.
        assert_eq!(default_manifest().version, Some(MANIFEST_VERSION));
    }

    #[test]
    fn default_manifest_reproduces_bundled_behavior() {
        // The default must: deploy ALL agents/skills from the BUNDLED source,
        // enable every instruction layer, select the professional style, and
        // leave model tiers unset — i.e. exactly the pre-HR-2 behavior.
        let m = default_manifest();

        let agents = m.agents.expect("agents set");
        assert!(agents.include.is_empty(), "empty include = all agents");
        assert!(agents.exclude.is_empty());
        assert_eq!(agents.source, ContentSource::Bundled);

        let skills = m.skills.expect("skills set");
        assert!(skills.include.is_empty(), "empty include = all skills");
        assert_eq!(skills.source, ContentSource::Bundled);

        let instr = m.instructions.expect("instruction layers");
        assert_eq!(instr.system, Some(true));
        assert_eq!(instr.contextual, Some(true));
        assert_eq!(instr.domain, Some(true));

        assert_eq!(
            m.style.and_then(|s| s.active),
            Some(crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID.to_string())
        );

        let models = m.models.expect("model tiers");
        assert_eq!(
            models,
            ModelTiers::default(),
            "no tier overrides by default"
        );
    }

    #[test]
    fn default_manifest_enables_both_mcp() {
        // Both MCP servers must be on by default, matching the unconditional
        // injection prepare_session did before HR-2.
        let mcp = default_manifest().mcp.expect("mcp section");
        assert_eq!(mcp.trusty_memory, Some(true));
        assert_eq!(mcp.trusty_search, Some(true));
    }
}
