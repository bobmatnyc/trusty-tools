//! The Architect's framework agents, written by `tm fleet init` (#10161 D3c).
//!
//! Why: the Architect dispatches its own agents (a read-only runner, the event
//! extractor, the improver, the improvement curator and the drift guard). The
//! bundled agent tier and `~/.claude/agents` load in every PM session, so the
//! one valid destination is the Architect's own `<project>/.claude/agents`
//! (rulings df-D21, df-D22, as for the skills). A re-run of `tm fleet init`
//! must refresh a copy nobody touched and keep an owner's edit.
//! What: [`SHIPPED_AGENTS`] names each agent and its shipped history; its
//! source is [`ShippedAgent::source`] under `instructions/`, read at runtime by
//! [`super::architect_seed`]. [`deploy_architect_agents`] writes them through
//! the skills' deploy loop ([`deploy_into`]) with [`AGENT_LAYOUT`], so the
//! shipped-history and owner-edit rule is the same code. A tier that resolves
//! onto a PM agent tier is refused and nothing is written.
//! Test: `architect_agents_tests.rs`, and in `tm fleet` tests
//! `the_architect_agents_reach_only_the_architect_project`.

use std::path::{Path, PathBuf};

use super::architect_skills::{
    ArchitectSkill, DeployReport, Layout, deploy_into, reserved_conflict,
};
use crate::core::paths::FrameworkPaths;

/// The content directory of the Architect's agents, relative to `instructions/`.
pub const AGENTS_DIR: &str = "architect/agents";

/// The deploy record, beside the agents in the project tier.
pub const AGENT_RECORD_FILE: &str = ".trusty-mpm-architect-agents.json";

/// One shipped agent: its name and its shipped history.
#[derive(Debug, Clone, Copy)]
pub struct ShippedAgent {
    /// The agent's `name:`, its file stem under `.claude/agents/`.
    pub name: &'static str,
    /// sha256 hex of every earlier shipped text; see [`ArchitectSkill::previous`].
    pub previous: &'static [&'static str],
}

impl ShippedAgent {
    /// The agent's source, relative to `instructions/`.
    pub fn source(&self) -> String {
        format!("{AGENTS_DIR}/{}.md", self.name)
    }
}

/// An agent on its first release: no earlier shipped text to recognise.
const fn first_release(name: &'static str) -> ShippedAgent {
    ShippedAgent {
        name,
        previous: &[],
    }
}

/// Every agent the Architect receives, in write order. A new agent is one
/// entry here plus `content/instructions/<AGENTS_DIR>/<name>.md`; edit an
/// agent's text and its old hash joins `previous`
/// (`the_shipped_agent_history_is_pinned_so_an_asset_edit_trips`).
// #10161 Q1 a: `token-optimizer`, the sixth, ships with the usage set (D3d).
pub const SHIPPED_AGENTS: &[ShippedAgent] = &[
    first_release("runner"),
    first_release("event-extractor"),
    first_release("improver"),
    first_release("improvement-curator"),
    first_release("drift-guard"),
];

/// The Architect's agent tier: `<project_dir>/.claude/agents`.
pub fn agents_tier(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("agents")
}

/// Where `name`'s file lives under `agents_dir`.
pub fn agent_path(agents_dir: &Path, name: &str) -> PathBuf {
    agents_dir.join(format!("{name}.md"))
}

/// The agents' layout: `<tier>/<name>.md`, recorded in [`AGENT_RECORD_FILE`].
pub const AGENT_LAYOUT: Layout = Layout {
    path: agent_path,
    record: AGENT_RECORD_FILE,
    kind: "agent",
};

/// The tiers an Architect agent must never reach: the bundled agent tier
/// every PM loads, and the operator's `~/.claude/agents`.
pub fn reserved_agent_tiers(fw: &FrameworkPaths) -> Vec<PathBuf> {
    vec![fw.agent_deploy_dir(), fw.claude_agents_dir()]
}

/// Write `agents` into `project_dir`'s agent tier, unless that tier resolves
/// onto one of `reserved`.
///
/// Why/What: see the module doc; a refused tier is the report's one error line
/// and nothing is written.
/// Test: `the_five_agents_are_written_byte_equal_and_recorded`,
/// `an_agent_tier_resolving_onto_a_pm_tier_is_refused`,
/// `a_second_init_follows_the_shipped_history_and_keeps_an_edit`.
pub fn deploy_architect_agents(
    project_dir: &Path,
    reserved: &[PathBuf],
    agents: &[ArchitectSkill],
) -> DeployReport {
    let tier = agents_tier(project_dir);
    if let Some(why) = reserved_conflict(&tier, reserved, AGENT_LAYOUT.kind) {
        return DeployReport {
            errors: vec![why],
            ..DeployReport::default()
        };
    }
    deploy_into(&tier, agents, &AGENT_LAYOUT)
}

#[cfg(test)]
#[path = "architect_agents_tests.rs"]
mod tests;
