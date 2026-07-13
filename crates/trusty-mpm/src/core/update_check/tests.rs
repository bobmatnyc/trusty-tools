//! Unit tests for catalog staleness detection (HR-3).
//!
//! Why: DOC-17 §HR-3 requires deterministic, offline staleness detection — these
//! tests pin the contract (stale on change, not-stale on identical, unknown when
//! never synced, selection filtering, change-list cap) without any network or a
//! live `claude`.
//! What: seed a catalog source tree on disk, build a deployed manifest whose
//! checksums match (or do not match) the composed/raw content, and assert the
//! [`StalenessReport`] flags + change list.
//! Test: this IS the test module.

use super::*;
use crate::core::agent_manifest::{AgentManifest, ManifestEntry, Origin, checksum};
use crate::core::skill_manifest::{SkillManifest, SkillManifestEntry};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Write a single self-contained (no `extends:`) agent file into `dir`.
fn write_agent(dir: &Path, stem: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{stem}.md")),
        format!("---\nname: {stem}\nrole: {stem}\n---\n\n# {stem}\n\n{body}\n"),
    )
    .unwrap();
}

/// Write a flat skill file into `dir`.
fn write_skill(dir: &Path, stem: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{stem}.md")),
        format!("# {stem}\n\n{body}\n"),
    )
    .unwrap();
}

/// Build a deployed agent manifest whose entry matches the COMPOSED catalog
/// agent (i.e. as if it had just been deployed from this exact catalog).
fn deployed_agent_matching(catalog_agents: &Path, stem: &str) -> AgentManifest {
    let composed = compose_agent(stem, catalog_agents).unwrap();
    let mut m = AgentManifest::default();
    m.managed.insert(
        format!("{stem}.md"),
        ManifestEntry {
            source_chain: vec![stem.to_owned()],
            checksum: checksum(&composed),
            deployed_at: "2026-06-17T00:00:00Z".to_owned(),
            origin: Origin::Bundled,
        },
    );
    m
}

/// Build a deployed skill manifest whose entry matches the RAW catalog skill.
fn deployed_skill_matching(catalog_skills: &Path, stem: &str) -> SkillManifest {
    let body = fs::read_to_string(catalog_skills.join(format!("{stem}.md"))).unwrap();
    let mut m = SkillManifest::default();
    m.managed.insert(
        stem.to_owned(),
        SkillManifestEntry {
            checksum: checksum(&body),
            deployed_at: "2026-06-17T00:00:00Z".to_owned(),
        },
    );
    m
}

#[test]
fn detect_unknown_when_never_synced() {
    // No catalog source trees exist (never synced). The report must be
    // `unknown` and NOT stale — DOC-17: never fabricate staleness, never block.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("repo/.claude/agents");
    let skills = root.path().join("repo/.claude/skills");

    let report = detect_staleness(
        &agents,
        &skills,
        &AgentManifest::default(),
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(report.unknown, "never-synced must be unknown");
    assert!(!report.stale, "never-synced must not be stale");
    assert!(report.changes.is_empty());
}

#[test]
fn detect_not_stale_when_identical() {
    // A catalog whose content exactly matches the deployed checksums is NOT
    // stale, even though the source trees exist (so not `unknown` either).
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    let skills = root.path().join("skills");
    write_agent(&agents, "rust-engineer", "ENGINEER BODY");
    write_skill(&skills, "tm-doctor", "DOCTOR BODY");

    let dep_agents = deployed_agent_matching(&agents, "rust-engineer");
    let dep_skills = deployed_skill_matching(&skills, "tm-doctor");

    let report = detect_staleness(
        &agents,
        &skills,
        &dep_agents,
        &dep_skills,
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(!report.unknown, "synced catalog is not unknown");
    assert!(!report.stale, "identical content is not stale: {report:?}");
    assert!(report.changes.is_empty());
}

#[test]
fn detect_flags_changed_agent() {
    // Deploy matches the catalog, THEN the catalog agent changes upstream. The
    // report must be stale with a single `agent ...: changed` entry.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    let skills = root.path().join("skills");
    write_agent(&agents, "rust-engineer", "ORIGINAL BODY");

    // Baseline deploy matches the original catalog content.
    let dep_agents = deployed_agent_matching(&agents, "rust-engineer");

    // Upstream edit: the catalog agent body changes.
    write_agent(&agents, "rust-engineer", "UPSTREAM EDIT — NEW BEHAVIOUR");

    let report = detect_staleness(
        &agents,
        &skills,
        &dep_agents,
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(report.stale, "changed agent must be stale");
    assert!(!report.unknown);
    assert_eq!(report.changes.len(), 1, "exactly one change: {report:?}");
    assert_eq!(report.changes[0].artifact, "agent");
    assert_eq!(report.changes[0].name, "rust-engineer");
    assert_eq!(report.changes[0].kind, ChangeKind::Changed);
}

#[test]
fn detect_flags_new_agent() {
    // A catalog agent absent from the deployed manifest is `new` → stale.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    write_agent(&agents, "newcomer", "BRAND NEW");

    let report = detect_staleness(
        &agents,
        &root.path().join("skills"),
        &AgentManifest::default(),
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(report.stale);
    assert_eq!(report.changes[0].kind, ChangeKind::New);
    assert_eq!(report.changes[0].name, "newcomer");
}

#[test]
fn detect_flags_new_skill() {
    // A catalog skill absent from the deployed skill manifest is `new` → stale.
    let root = TempDir::new().unwrap();
    let skills = root.path().join("skills");
    write_skill(&skills, "fresh-skill", "NEW SKILL");

    let report = detect_staleness(
        &root.path().join("agents"),
        &skills,
        &AgentManifest::default(),
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(report.stale);
    assert_eq!(report.changes.len(), 1);
    assert_eq!(report.changes[0].artifact, "skill");
    assert_eq!(report.changes[0].name, "fresh-skill");
    assert_eq!(report.changes[0].kind, ChangeKind::New);
}

#[test]
fn detect_flags_changed_skill() {
    // Deploy matches, then the catalog skill body changes → `skill ...: changed`.
    let root = TempDir::new().unwrap();
    let skills = root.path().join("skills");
    write_skill(&skills, "tm-doctor", "ORIGINAL");
    let dep_skills = deployed_skill_matching(&skills, "tm-doctor");
    write_skill(&skills, "tm-doctor", "UPSTREAM CHANGED");

    let report = detect_staleness(
        &root.path().join("agents"),
        &skills,
        &AgentManifest::default(),
        &dep_skills,
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(report.stale);
    assert_eq!(report.changes[0].kind, ChangeKind::Changed);
    assert_eq!(report.changes[0].artifact, "skill");
}

#[test]
fn detect_respects_selection() {
    // An agent the selection predicate REJECTS must not count toward staleness,
    // even when it differs from (here: is absent from) the deployed manifest.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    write_agent(&agents, "rust-engineer", "BODY");
    write_agent(&agents, "php-engineer", "BODY");

    // Deploy matches rust-engineer; php-engineer is new BUT deselected.
    let dep_agents = deployed_agent_matching(&agents, "rust-engineer");

    let report = detect_staleness(
        &agents,
        &root.path().join("skills"),
        &dep_agents,
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |name| name == "rust-engineer", // exclude php-engineer
        |_| true,
        |_| false,
    );
    assert!(
        !report.stale,
        "a deselected new agent must not make the harness stale: {report:?}"
    );
}

#[test]
fn detect_respects_ignore_staleness() {
    // Issue #2462: an agent matched by `ignore_staleness` must not count toward
    // staleness, even though it is fully SELECTED (unlike `detect_respects_
    // selection`, which tests the deploy-roster predicate). This is the
    // narrower, deploy-preserving exemption `AgentSet::ignore_staleness` adds.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    write_agent(&agents, "rust-engineer", "BODY");
    write_agent(&agents, "engineer", "BODY");

    // Deploy matches rust-engineer; engineer is new (never deployed) but the
    // operator has flagged it as a known, intentional customization.
    let dep_agents = deployed_agent_matching(&agents, "rust-engineer");

    let report = detect_staleness(
        &agents,
        &root.path().join("skills"),
        &dep_agents,
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true, // both agents SELECTED (deploy roster unaffected)
        |_| true,
        |name| name == "engineer", // but staleness-exempt
    );
    assert!(
        !report.stale,
        "an ignore_staleness agent must not make the harness stale: {report:?}"
    );
}

#[test]
fn detect_caps_change_list() {
    // Many new agents must still set `stale`, but the change list is capped at
    // MAX_CHANGES so the health payload stays small.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    for i in 0..(MAX_CHANGES + 8) {
        write_agent(&agents, &format!("agent{i:02}"), "BODY");
    }

    let report = detect_staleness(
        &agents,
        &root.path().join("skills"),
        &AgentManifest::default(),
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(report.stale);
    assert_eq!(
        report.changes.len(),
        MAX_CHANGES,
        "change list must be capped"
    );
}

#[test]
fn summary_lines_render_kind_and_name() {
    // The human summary must read `<artifact> <name>: <kind>`.
    let report = StalenessReport {
        stale: true,
        unknown: false,
        changes: vec![
            CatalogChange {
                artifact: "agent",
                name: "rust-engineer".into(),
                kind: ChangeKind::Changed,
            },
            CatalogChange {
                artifact: "skill",
                name: "tm-doctor".into(),
                kind: ChangeKind::New,
            },
        ],
    };
    let lines = report.summary_lines();
    assert_eq!(lines[0], "agent rust-engineer: changed");
    assert_eq!(lines[1], "skill tm-doctor: new");
}

#[test]
fn detect_for_framework_unknown_without_catalog() {
    // With the compiled-in default manifest (bundled sources) and an empty
    // framework root, the catalog source trees do not exist, so the framework-
    // level detection reports `unknown` — never a fabricated staleness.
    let fw_root = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(fw_root.path());
    let report = detect_for_framework(&fw, fw_root.path());
    assert!(
        report.unknown,
        "bundled default with no catalog must be unknown: {report:?}"
    );
    assert!(!report.stale);
}

#[test]
fn detect_unknown_when_only_one_tree_missing_is_not_unknown() {
    // If at least ONE source tree exists, the catalog has been synced; the report
    // is a real comparison (not `unknown`) over whichever tree is present.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    write_agent(&agents, "solo", "BODY"); // skills tree absent

    let report = detect_staleness(
        &agents,
        &root.path().join("skills"), // does not exist
        &AgentManifest::default(),
        &SkillManifest::default(),
        &root.path().join("dep-agents"),
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(
        !report.unknown,
        "one present tree means synced, not unknown"
    );
    assert!(report.stale, "the present agent is new → stale");
}

/// Write the COMPOSED catalog agent to a deployment dir, simulating a file tm
/// deployed WITHOUT recording a manifest entry (the #1940 root-cause scenario).
fn deploy_agent_on_disk(catalog_agents: &Path, deployed_dir: &Path, stem: &str) {
    let composed = compose_agent(stem, catalog_agents).unwrap();
    fs::create_dir_all(deployed_dir).unwrap();
    fs::write(deployed_dir.join(format!("{stem}.md")), composed).unwrap();
}

/// Write the RAW catalog skill to `<deployed_dir>/<stem>/SKILL.md`, matching the
/// skill deployer's on-disk layout, again WITHOUT a manifest entry.
fn deploy_skill_on_disk(catalog_skills: &Path, deployed_dir: &Path, stem: &str) {
    let body = fs::read_to_string(catalog_skills.join(format!("{stem}.md"))).unwrap();
    let skill_dir = deployed_dir.join(stem);
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), body).unwrap();
}

#[test]
fn detect_reconciles_deployed_but_unmanifested() {
    // #1940: an agent/skill already deployed on disk with the current catalog
    // content but ABSENT from the checksum manifest must NOT be reported as `new`.
    // A fully-deployed roster reports 0 changes even with an empty manifest.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    let skills = root.path().join("skills");
    write_agent(&agents, "rust-engineer", "ENGINEER BODY");
    write_skill(&skills, "tm-doctor", "DOCTOR BODY");

    let dep_agents_dir = root.path().join("dep-agents");
    let dep_skills_dir = root.path().join("dep-skills");
    deploy_agent_on_disk(&agents, &dep_agents_dir, "rust-engineer");
    deploy_skill_on_disk(&skills, &dep_skills_dir, "tm-doctor");

    // Manifests are EMPTY — the deploy path never recorded them (the live bug).
    let report = detect_staleness(
        &agents,
        &skills,
        &AgentManifest::default(),
        &SkillManifest::default(),
        &dep_agents_dir,
        &dep_skills_dir,
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(!report.unknown);
    assert!(
        !report.stale,
        "deployed-but-unmanifested roster must reconcile to 0 changes: {report:?}"
    );
    assert!(report.changes.is_empty());
}

#[test]
fn detect_user_owned_on_disk_not_reported() {
    // A file on disk that differs from the catalog content and is NOT managed is
    // user-owned; `apply` skips it, so it must not be reported as drift.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    write_agent(&agents, "rust-engineer", "CATALOG BODY");

    let dep_agents_dir = root.path().join("dep-agents");
    fs::create_dir_all(&dep_agents_dir).unwrap();
    fs::write(
        dep_agents_dir.join("rust-engineer.md"),
        "USER-OWNED DIFFERENT CONTENT",
    )
    .unwrap();

    let report = detect_staleness(
        &agents,
        &root.path().join("skills"),
        &AgentManifest::default(),
        &SkillManifest::default(),
        &dep_agents_dir,
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(
        !report.stale,
        "a user-owned on-disk file `apply` never touches must not be drift: {report:?}"
    );
}

#[test]
fn detect_user_modified_managed_not_reported() {
    // A MANAGED agent whose catalog content changed, but whose on-disk copy the
    // user has since edited (differs from the recorded checksum), must NOT be
    // reported — `apply` preserves the user's edit rather than refreshing it.
    let root = TempDir::new().unwrap();
    let agents = root.path().join("agents");
    write_agent(&agents, "rust-engineer", "ORIGINAL");
    let dep_agents = deployed_agent_matching(&agents, "rust-engineer");
    // Upstream edit: catalog content changes.
    write_agent(&agents, "rust-engineer", "UPSTREAM CHANGED");
    // On disk the user has hand-edited it (matches neither recorded nor catalog).
    let dep_agents_dir = root.path().join("dep-agents");
    fs::create_dir_all(&dep_agents_dir).unwrap();
    fs::write(dep_agents_dir.join("rust-engineer.md"), "USER HAND-EDIT").unwrap();

    let report = detect_staleness(
        &agents,
        &root.path().join("skills"),
        &dep_agents,
        &SkillManifest::default(),
        &dep_agents_dir,
        &root.path().join("dep-skills"),
        |_| true,
        |_| true,
        |_| false,
    );
    assert!(
        !report.stale,
        "a user-modified managed file `apply` skips must not be drift: {report:?}"
    );
}

#[test]
fn classify_drift_reconciles_on_disk() {
    // Unit-level: on-disk content matching the catalog hash is never drift, even
    // with no manifest entry; a managed+unmodified changed file is Changed.
    let cat = checksum("catalog content");
    // Deployed-but-unmanifested and current → None (reconcile).
    assert_eq!(
        super::classify_drift(&cat, None, Some("catalog content")),
        None
    );
    // Managed, unmodified on disk, catalog changed → Changed.
    let recorded = checksum("old content");
    assert_eq!(
        super::classify_drift(&cat, Some(&recorded), Some("old content")),
        Some(ChangeKind::Changed)
    );
    // Managed, user-modified on disk, catalog changed → None (apply skips).
    assert_eq!(
        super::classify_drift(&cat, Some(&recorded), Some("user edit")),
        None
    );
}

#[test]
fn classify_drift_new_when_absent() {
    // Nothing on disk and no manifest entry → genuinely new (apply deploys it).
    let cat = checksum("catalog content");
    assert_eq!(
        super::classify_drift(&cat, None, None),
        Some(ChangeKind::New)
    );
    // Manifest already records the catalog content → not drift regardless of disk.
    assert_eq!(super::classify_drift(&cat, Some(&cat), None), None);
}
