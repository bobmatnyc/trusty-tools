//! Tests for the #8534 critic round 3 finding: a user's gitignored agent or
//! skill keeps its worktree; only what tm's ledgers record goes with it.
//! Each keep test fails at 26b7c15ca, which excused `.claude/agents/` and
//! `.claude/skills/*` by name.
//! Test: this file IS the test module.

use std::path::Path;

use crate::core::agent_manifest::{AgentManifest, ManifestEntry, Origin, checksum};
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};
use crate::session_manager::decommission_force::{ProvisioningDirt, remove_in_project_worktree};
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ignored_output::kept_ignored_output;

const SKILL_BODY: &str = "---\nname: deploy-check\n---\n# Deploy check\n";
const REFERENCE_BODY: &str = "# Reference\n";
const AGENT_BODY: &str = "# Engineer\n";

/// Ignore the deploy directories as tm's scaffold block does, plus the
/// ownership marker, for every worktree of `fx`'s repository.
fn ignore_deploy_dirs(fx: &GitWorktreeFixture) {
    std::fs::write(
        fx.repo.join(".git/info/exclude"),
        "/.claude/agents/\n/.claude/skills/*\n/.trusty-mpm-worktree\n",
    )
    .expect("write info/exclude");
}

/// Write `body` to `wt/rel`, creating its parent directories.
fn put(wt: &Path, rel: &str, body: &str) {
    let path = wt.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, body).expect("write");
}

/// A clean tree holding the `deploy-check` skill: `SKILL.md` and a reference.
fn tree_with_skill(fx: &GitWorktreeFixture, name: &str) -> std::path::PathBuf {
    ignore_deploy_dirs(fx);
    let wt = fx.add_worktree(name);
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    put(&wt, ".claude/skills/deploy-check/SKILL.md", SKILL_BODY);
    put(
        &wt,
        ".claude/skills/deploy-check/references/steps.md",
        REFERENCE_BODY,
    );
    wt
}

/// Record `entries` (`key`, deployed content) in `wt`'s skills ledger.
fn record_skills(wt: &Path, entries: &[(&str, &str)]) {
    let mut ledger = SkillManifest::default();
    for (key, body) in entries {
        ledger.managed.insert(
            (*key).to_string(),
            SkillManifestEntry {
                checksum: checksum(body),
                deployed_at: "2026-09-26T00:00:00Z".to_string(),
            },
        );
    }
    ledger
        .save(&wt.join(".claude/skills"))
        .expect("save skills ledger");
}

/// Both files of a ledger-recorded skill, unchanged.
fn record_deploy_check(wt: &Path) {
    record_skills(
        wt,
        &[
            ("deploy-check", SKILL_BODY),
            ("deploy-check/references/steps.md", REFERENCE_BODY),
        ],
    );
}

/// 🔴 A skill the user wrote keeps the tree, through decommission itself.
#[tokio::test]
async fn a_user_skill_keeps_the_tree() {
    let fx = GitWorktreeFixture::new();
    let wt = tree_with_skill(&fx, "user-skill-8534");

    let kept = kept_ignored_output(&wt)
        .expect("the check completes")
        .expect("a user skill is kept output");
    assert_eq!(
        (kept.files, kept.first.as_str()),
        (2, ".claude/skills/deploy-check/")
    );

    let verdict =
        remove_in_project_worktree(&ManagedSessionId::new(), &wt, ProvisioningDirt::Refuse).await;
    assert!(!verdict.removed, "{verdict:?}");
    assert!(wt.join(".claude/skills/deploy-check/SKILL.md").exists());
}

/// A skill tm's ledger records, unchanged since, does not block removal.
#[tokio::test]
async fn a_manifest_named_skill_does_not_block_removal() {
    let fx = GitWorktreeFixture::new();
    let wt = tree_with_skill(&fx, "tm-skill-8534");
    record_deploy_check(&wt);
    put(
        &wt,
        ".claude/skills/.trusty-mpm-skills-manifest.json.lock",
        "",
    );
    put(&wt, ".claude/skills/.trusty-mpm-project-tier-stamp", "abc");

    assert_eq!(kept_ignored_output(&wt), Ok(None));
    let verdict =
        remove_in_project_worktree(&ManagedSessionId::new(), &wt, ProvisioningDirt::Refuse).await;
    assert!(verdict.removed, "{:?}", verdict.kept_reason);
}

/// A file the user added inside a tm skill, or a tm file they edited, is kept.
#[test]
fn a_hand_edited_deployed_skill_keeps_the_tree() {
    let fx = GitWorktreeFixture::new();
    let wt = tree_with_skill(&fx, "edited-skill-8534");
    record_deploy_check(&wt);
    put(&wt, ".claude/skills/deploy-check/notes.md", "mine");

    let kept = kept_ignored_output(&wt).expect("the check completes");
    assert_eq!(kept.map(|k| k.files), Some(1), "only the added file");

    std::fs::remove_file(wt.join(".claude/skills/deploy-check/notes.md")).expect("rm notes");
    put(
        &wt,
        ".claude/skills/deploy-check/SKILL.md",
        "edited by hand\n",
    );
    let kept = kept_ignored_output(&wt).expect("the check completes");
    assert_eq!(kept.map(|k| k.files), Some(1), "only the edited file");
}

/// Fail-safe: a skills ledger that cannot be read excuses nothing.
#[test]
fn an_unreadable_skill_manifest_keeps_the_tree() {
    let fx = GitWorktreeFixture::new();
    let wt = tree_with_skill(&fx, "torn-ledger-8534");
    put(
        &wt,
        ".claude/skills/.trusty-mpm-skills-manifest.json",
        "{ torn",
    );

    let kept = kept_ignored_output(&wt)
        .expect("the check completes")
        .expect("an unreadable ledger keeps every skill file");
    assert_eq!(kept.files, 2);
}

/// Record `name` in `wt`'s agent ledger with `origin`.
fn record_agent(ledger: &mut AgentManifest, name: &str, origin: Origin) {
    ledger.managed.insert(
        name.to_string(),
        ManifestEntry {
            source_chain: vec!["engineer".to_string()],
            checksum: checksum(AGENT_BODY),
            deployed_at: "2026-09-26T00:00:00Z".to_string(),
            origin,
        },
    );
}

/// An agent file is tm's only when the ledger records it with a framework
/// origin; a user-origin entry and an unrecorded file are kept.
#[test]
fn only_framework_agents_in_the_ledger_are_excused() {
    let fx = GitWorktreeFixture::new();
    ignore_deploy_dirs(&fx);
    let wt = fx.add_worktree("agents-8534");
    let mut ledger = AgentManifest::default();
    record_agent(&mut ledger, "engineer.md", Origin::Bundled);
    ledger
        .save(&wt.join(".claude/agents"))
        .expect("save agents ledger");
    put(&wt, ".claude/agents/engineer.md", AGENT_BODY);
    assert_eq!(kept_ignored_output(&wt), Ok(None));

    put(&wt, ".claude/agents/mine.md", AGENT_BODY);
    let kept = kept_ignored_output(&wt).expect("the check completes");
    assert_eq!(kept.map(|k| k.files), Some(1), "an unrecorded agent");

    record_agent(&mut ledger, "mine.md", Origin::User);
    ledger
        .save(&wt.join(".claude/agents"))
        .expect("save agents ledger");
    let kept = kept_ignored_output(&wt).expect("the check completes");
    assert_eq!(kept.map(|k| k.files), Some(1), "a user-origin agent");
}
