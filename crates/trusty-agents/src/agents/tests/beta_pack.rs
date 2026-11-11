//! The beta overlay config pack: Opus 5.5 main agent, Sonnet 5.5 sub-agent,
//! both pinned to Bedrock (#10979, epic #10977 M1-T3).
//!
//! Why: no shipped config gave an Opus main agent and a Sonnet sub-agent on
//! Bedrock. The pack lives outside the bundled roster
//! (`content/agents/trusty-agents/overlays/beta/`) so it is never deployed to
//! `~/.trusty-agents/agents`; the launch recipe points `TAGENT_CONFIG_DIR` at
//! it, which puts it ahead of the deployed tier in `agents_dir_candidates`.
//! The child is named `research-agent`, not `beta-researcher`, because the
//! ADR-0024 name floor (`ASSISTANT_REACHABLE_SUBAGENTS`) would drop any other
//! name (Architect ruling, option A).
//! What: loads both agents through `AgentConfig::by_name_in` over
//! `[overlay, bundled]` and checks the exact slugs, the Bedrock pin, the AWS
//! profile, the palace store and the reachable sub-agent set.
//! Test: `beta_pack_parent_is_opus_on_bedrock_and_reaches_only_the_pinned_child`,
//! `beta_pack_child_is_sonnet_on_bedrock_and_tracks_the_bundled_research_agent`.

use std::path::PathBuf;

use trusty_common::inference::registry::ProviderId;

use super::loading::ENV_LOCK;
use crate::agents::bundled::BundledTreeKind;
use crate::agents::bundled::content::test_support::repo_tree_dir;
use crate::agents::{AgentConfig, RunnerKind};
use crate::tools::subagent_allow::SubagentAllowSet;

const OPUS: &str = "bedrock/us.anthropic.claude-opus-5-5";
const SONNET: &str = "bedrock/us.anthropic.claude-sonnet-5-5";

/// `[overlay, bundled]` — the order `TAGENT_CONFIG_DIR=<overlay>` produces.
fn search_dirs() -> Vec<PathBuf> {
    let bundled = repo_tree_dir(BundledTreeKind::Agents);
    let overlay = bundled
        .parent()
        .expect("the bundled agents tree has a parent")
        .join("overlays")
        .join("beta");
    vec![overlay, bundled]
}

/// Loads `name` with no model-override env var in play.
fn load(name: &str) -> AgentConfig {
    let _guard = ENV_LOCK.blocking_lock();
    let suffix = crate::agents::agent_env_suffix(name);
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var(format!("TAGENT_MODEL_{suffix}"));
        std::env::remove_var("TAGENT_DEFAULT_MODEL");
    }
    AgentConfig::by_name_in(&search_dirs(), name)
        .unwrap_or_else(|e| panic!("beta pack agent '{name}' must load: {e:#}"))
}

/// The pin must resolve to Bedrock, and the slug must already carry it.
fn assert_bedrock_pin(cfg: &AgentConfig, slug: &str) {
    assert_eq!(cfg.agent.model, slug, "'{}' slug", cfg.agent.name);
    assert_eq!(cfg.agent.provider_id.as_deref(), Some("bedrock"));
    assert_eq!(
        crate::llm::provider_pin::resolve(&cfg.agent.name, cfg.agent.provider_id.as_deref()),
        Ok(Some(ProviderId::Bedrock))
    );
    assert!(
        !cfg.llm.use_anthropic_direct,
        "a Bedrock pin never dials Anthropic"
    );
    assert_ne!(
        cfg.agent.runner,
        RunnerKind::ClaudeCode,
        "'{}' must not route through the Claude Code CLI",
        cfg.agent.name
    );
    assert_eq!(cfg.llm.aws_profile.as_deref(), Some("cto"));
    assert_eq!(cfg.llm.aws_region.as_deref(), Some("us-east-1"));
}

#[test]
fn beta_pack_parent_is_opus_on_bedrock_and_reaches_only_the_pinned_child() {
    let parent = load("beta-assistant");
    assert_bedrock_pin(&parent, OPUS);
    assert_eq!(parent.agent.role, "assistant");
    assert!(
        parent.llm.max_tokens > 1024,
        "the pack raises the base assistant's 1024-token reply cap, got {}",
        parent.llm.max_tokens
    );
    let store = parent
        .stores
        .primary()
        .expect("the beta assistant declares a [[stores]] binding");
    assert_eq!(store.palace.as_deref(), Some("beta-assistant"));

    // `replace_subagents` drops the base's union, so only the pinned child is
    // listed, and the ADR-0024 floor admits it.
    assert_eq!(
        parent.subagents.delegate_allowed.as_deref(),
        Some(&["research-agent".to_string()][..])
    );
    let reach = SubagentAllowSet::over(
        crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS,
        parent.subagents.delegate_allowed.as_deref(),
    );
    assert!(reach.resolve("research-agent").is_ok());
    assert!(
        reach.resolve("ticketing-agent").is_err(),
        "the unpinned bundled ticketing-agent must not be reachable"
    );
}

#[test]
fn beta_pack_child_is_sonnet_on_bedrock_and_tracks_the_bundled_research_agent() {
    let child = load("research-agent");
    assert_bedrock_pin(&child, SONNET);

    // The overlay is a copy of the bundled agent with only routing changed;
    // a drifted prompt or tool list would make the beta test a different agent.
    let bundled =
        AgentConfig::by_name_in(&[repo_tree_dir(BundledTreeKind::Agents)], "research-agent")
            .expect("the bundled research-agent loads");
    assert_eq!(child.agent.role, bundled.agent.role);
    assert_eq!(child.system_prompt.content, bundled.system_prompt.content);
    assert_eq!(child.tools.allowed, bundled.tools.allowed);
    assert_eq!(child.tools.scopes, bundled.tools.scopes);
}
