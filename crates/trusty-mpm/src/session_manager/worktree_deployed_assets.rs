//! Which gitignored agent and skill files tm deployed into a worktree (#8534).
//!
//! Why: `.claude/agents/` and `.claude/skills/*` are gitignored by tm's
//! scaffold block, but they also hold a user's own agents and skills. Excusing
//! the directories by name let decommission delete a user-written
//! `.claude/skills/deploy-check/SKILL.md` (#8534 critic round 3).
//! What: a file is tm's only when the ledger tm keeps beside it records it —
//! the skills manifest in `.claude/skills/`, the agent manifest in
//! `.claude/agents/` with a framework origin — and its bytes still match the
//! recorded checksum. Everything else under those directories is kept output,
//! and so is every file there when its ledger cannot be read.
//! Test: `worktree_ignored_output_asset_tests`.

use std::path::Path;

use tracing::warn;

use crate::core::agent_manifest::{AgentManifest, ManifestLoad};
use crate::core::skill_manifest::SkillManifest;

/// The skills deploy directory, repo-relative.
const SKILLS_DIR: &str = ".claude/skills";
/// The agents deploy directory, repo-relative.
const AGENTS_DIR: &str = ".claude/agents";

/// Is `rel` one of the deploy directories or beneath one?
///
/// Why: those paths are decided by a ledger, so the static harness list must
/// not excuse them by name.
/// Test: `harness_paths_are_excused_and_look_alikes_are_not`.
pub(super) fn is_asset_path(rel: &str) -> bool {
    let rel = rel.trim_end_matches('/');
    [SKILLS_DIR, AGENTS_DIR].iter().any(|dir| {
        rel.strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// The two ledgers of one worktree, each read at most once per check.
pub(super) struct DeployedAssets<'a> {
    root: &'a Path,
    skills: Option<Option<SkillManifest>>,
    agents: Option<Option<AgentManifest>>,
}

impl<'a> DeployedAssets<'a> {
    /// A classifier for the tree at `root`; nothing is read until asked.
    pub(super) fn new(root: &'a Path) -> Self {
        Self {
            root,
            skills: None,
            agents: None,
        }
    }

    /// Did tm deploy the file at `rel`, and is it unchanged since?
    ///
    /// Why: only such a file is regenerable — tm writes it back on the next
    /// deploy. A user's file, or tm's file after a hand edit, is work.
    /// What: `.claude/skills/<skill>/SKILL.md` is keyed `<skill>` and any other
    /// file in the skill `<skill>/<rest>`, as the skill deployer records them;
    /// `.claude/agents/<file>` is keyed `<file>` and needs a framework origin.
    /// An unreadable ledger or file, or any other path, answers `false`.
    /// Test: `a_user_skill_keeps_the_tree`,
    /// `a_manifest_named_skill_does_not_block_removal`,
    /// `an_unreadable_skill_manifest_keeps_the_tree`,
    /// `a_hand_edited_deployed_skill_keeps_the_tree`,
    /// `only_framework_agents_in_the_ledger_are_excused`.
    pub(super) fn is_tm_deployed(&mut self, rel: &str) -> bool {
        if let Some(rest) = rel.strip_prefix(SKILLS_DIR).and_then(under) {
            let Some((skill, file)) = rest.split_once('/') else {
                return false; // loose files in the directory are not a skill's
            };
            let key = if file == "SKILL.md" {
                skill.to_string()
            } else {
                format!("{skill}/{file}")
            };
            let root = self.root;
            let Some(ledger) = self.skills.get_or_insert_with(|| load_skills(root)) else {
                return false;
            };
            return read(root, rel).is_some_and(|body| ledger.checksum_matches(&key, &body));
        }
        if let Some(file) = rel.strip_prefix(AGENTS_DIR).and_then(under) {
            if file.contains('/') {
                return false; // the agent deployer writes flat files only
            }
            let root = self.root;
            let Some(ledger) = self.agents.get_or_insert_with(|| load_agents(root)) else {
                return false;
            };
            let framework = ledger
                .managed
                .get(file)
                .is_some_and(|entry| entry.origin.is_framework_owned());
            return framework
                && read(root, rel).is_some_and(|body| ledger.checksum_matches(file, &body));
        }
        false
    }
}

/// The rest of a path after `/`, if the prefix stripped was a whole segment.
fn under(rest: &str) -> Option<&str> {
    rest.strip_prefix('/').filter(|r| !r.is_empty())
}

/// `root/rel` as text, or `None` when it cannot be read or is not UTF-8.
fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

/// The skills ledger, or `None` (logged) when it exists but cannot be read.
fn load_skills(root: &Path) -> Option<SkillManifest> {
    SkillManifest::load(&root.join(SKILLS_DIR))
        .inspect_err(|e| {
            warn!(root = %root.display(), "skills ledger unreadable ({e}); keeping every skill file (#8534)");
        })
        .ok()
}

/// The agents ledger, or `None` (logged) when it exists but cannot be read.
fn load_agents(root: &Path) -> Option<AgentManifest> {
    match AgentManifest::load_checked(&root.join(AGENTS_DIR)) {
        ManifestLoad::Ok(ledger) => Some(ledger),
        ManifestLoad::Corrupt(e) => {
            warn!(root = %root.display(), "agents ledger unreadable ({e}); keeping every agent file (#8534)");
            None
        }
    }
}
