//! The Architect agents `tm fleet init` writes into `.claude/agents/` (#10161 D3c).
//!
//! Why: the agents must reach the Architect project only, and a re-run of
//! init must follow the skills' shipped-history and owner-edit rule, not the
//! plain "write when absent" rule of the other seed files.
//! What: [`steps`] runs [`deploy_architect_agents`] against the PM agent tiers
//! of both `home` and the process's own home, and turns its report into one
//! [`Step`] per agent plus one `Failed` step per error line.
//! Test: `the_architect_agents_reach_only_the_architect_project`,
//! `a_second_init_follows_the_shipped_history_and_keeps_an_edit`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::project_skill_tier::architect_agents::{
    deploy_architect_agents, reserved_agent_tiers,
};
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;

use super::Step;

/// The PM agent tiers under `home` and under the process's home.
fn reserved(home: &Path) -> Vec<PathBuf> {
    let mut tiers = reserved_agent_tiers(&FrameworkPaths::under(home));
    tiers.extend(reserved_agent_tiers(&FrameworkPaths::default()));
    tiers
}

/// Write the seed's agents under `dir`; one step per agent, one per error.
pub(crate) fn steps(dir: &Path, home: &Path, seed: &ArchitectSeed) -> Vec<Step> {
    let report = deploy_architect_agents(dir, &reserved(home), &seed.agents);
    let mut steps: Vec<Step> = seed
        .agents
        .iter()
        .map(|agent| {
            let dest = format!(".claude/agents/{}.md", agent.stem);
            let name = agent.stem.to_owned();
            if report.written.contains(&name) {
                Step::Changed(format!("wrote {dest}"))
            } else if report.kept_custom.contains(&name) {
                Step::Skipped(format!(
                    "{dest}: differs from the shipped copy (edited), so it is left as is"
                ))
            } else {
                Step::Unchanged(dest)
            }
        })
        .collect();
    if !report.errors.is_empty() {
        // A refused tier or a failed write: no agent line may read as current.
        steps.retain(|step| !matches!(step, Step::Unchanged(_)));
        steps.extend(report.errors.into_iter().map(Step::Failed));
    }
    steps
}
