//! Unit tests for `apply_catalog` (HR-3 rebuild offer).
//!
//! Why: the apply path must be verifiable offline — a `FakeGitBackend` simulates
//! the catalog checkout, the test seeds catalog agent/skill files into it (along
//! with a minimal `.git/config` so `ensure_repo` takes the idempotent update path),
//! and the assertions cover redeploy + staleness-clear and the opt-in prune
//! (including that prune spares user-owned files).
//! What: build a hermetic `FrameworkPaths` under a tempdir, write a project
//! manifest selecting the CATALOG source, run `apply_catalog`, and assert the
//! deployed files + a subsequent `detect_for_framework` reports fresh.
//! Test: this IS the test module.

use super::*;
use crate::core::update_check::detect_for_framework;
use crate::provisioner::FakeGitBackend;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Mark `<catalog>/repo` as a valid git checkout so `ensure_repo` takes the
/// idempotent update path (fetch+reset via `FakeGitBackend`, a no-op) rather
/// than treating the pre-seeded directory as corrupt and wiping it.
///
/// Why: `CatalogSync::ensure_repo` (fix for #1751) now checks for `.git/`
/// before deciding whether to clone or update in place. Test helpers that
/// pre-seed catalog content must create this marker so the seeded files survive
/// the sync call inside `apply_catalog`.
/// What: writes a minimal `.git/config` with the default catalog remote URL at
/// `<catalog>/repo/.git/config`, idempotently.
/// Test: called by the seeding helpers below.
fn mark_catalog_repo_as_valid(catalog_root: &Path) {
    let git_dir = catalog_root.join("repo/.git");
    fs::create_dir_all(&git_dir).unwrap();
    // Use the same URL as DEFAULT_CATALOG_REPO in catalog_sync.rs.
    fs::write(
        git_dir.join("config"),
        "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = https://github.com/bobmatnyc/claude-mpm\n",
    )
    .unwrap();
}

/// Seed a catalog agent file at the layout `CatalogSync` writes
/// (`<catalog>/repo/.claude/agents/<stem>.md`).
fn seed_catalog_agent(catalog_root: &Path, stem: &str, body: &str) {
    mark_catalog_repo_as_valid(catalog_root);
    let dir = catalog_root.join("repo/.claude/agents");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{stem}.md")),
        format!("---\nname: {stem}\nrole: {stem}\n---\n\n# {stem}\n\n{body}\n"),
    )
    .unwrap();
}

/// Seed a catalog skill file at `<catalog>/repo/.claude/skills/<stem>.md`.
fn seed_catalog_skill(catalog_root: &Path, stem: &str, body: &str) {
    mark_catalog_repo_as_valid(catalog_root);
    let dir = catalog_root.join("repo/.claude/skills");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{stem}.md")),
        format!("# {stem}\n\n{body}\n"),
    )
    .unwrap();
}

/// Write a project manifest selecting the CATALOG source for agents and skills.
fn write_catalog_manifest(project_dir: &Path) {
    let dir = project_dir.join(".trusty-mpm");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.toml"),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\n",
    )
    .unwrap();
}

#[test]
fn apply_redeploys_and_clears_staleness() {
    // A catalog-sourced manifest with new content must deploy that content and
    // leave the harness NOT stale (the checksum manifests now match the catalog).
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));
    write_catalog_manifest(project.path());

    // Pre-seed the catalog checkout content (the FakeGitBackend only mkdirs).
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_agent(&catalog_root, "rust-engineer", "ENGINEER BODY");
    seed_catalog_skill(&catalog_root, "tm-doctor", "DOCTOR BODY");

    let report = apply_catalog(
        FakeGitBackend::new(),
        &fw,
        project.path(),
        true,  // force sync
        false, // no prune
    )
    .unwrap();

    assert!(
        report
            .agents_deployed
            .contains(&"rust-engineer.md".to_string()),
        "agent deployed: {report:?}"
    );
    assert!(
        report.skills_deployed.contains(&"tm-doctor".to_string()),
        "skill deployed: {report:?}"
    );

    // The deployed files exist where Claude Code reads them.
    assert!(fw.claude_agents_dir().join("rust-engineer.md").is_file());
    assert!(
        fw.claude_skills_dir()
            .join("tm-doctor")
            .join("SKILL.md")
            .is_file()
    );

    // After apply, the framework-level staleness check reports fresh (not stale,
    // not unknown — the catalog tree exists and the deployed hashes match).
    let report = detect_for_framework(&fw, project.path());
    assert!(!report.stale, "apply must clear staleness: {report:?}");
    assert!(!report.unknown, "catalog is synced: {report:?}");
}

#[test]
fn apply_is_stale_before_apply_then_fresh_after() {
    // End-to-end staleness lifecycle: a catalog agent absent from the deployment
    // reads stale BEFORE apply, then fresh AFTER.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));
    write_catalog_manifest(project.path());

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_agent(&catalog_root, "newcomer", "NEW BODY");

    // BEFORE apply: the catalog has an agent the deployment lacks → stale.
    let before = detect_for_framework(&fw, project.path());
    assert!(
        before.stale,
        "new catalog agent is stale before apply: {before:?}"
    );

    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    // AFTER apply: deployed, so fresh.
    let after = detect_for_framework(&fw, project.path());
    assert!(!after.stale, "apply clears it: {after:?}");
}

#[test]
fn apply_prune_removes_deselected() {
    // After deploying two agents, a manifest that EXCLUDES one must, with --prune,
    // remove the deselected agent's deployed file and manifest entry.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_agent(&catalog_root, "keep-me", "KEEP");
    seed_catalog_agent(&catalog_root, "drop-me", "DROP");

    // First apply deploys BOTH (no exclude).
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();
    assert!(fw.claude_agents_dir().join("drop-me.md").is_file());

    // Now narrow the manifest to exclude drop-me, and apply WITH prune.
    fs::write(
        project.path().join(".trusty-mpm").join("manifest.toml"),
        "[agents]\nsource = \"catalog\"\nexclude = [\"drop-me\"]\n\n[skills]\nsource = \"catalog\"\n",
    )
    .unwrap();

    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();
    assert!(
        report.agents_pruned.contains(&"drop-me.md".to_string()),
        "drop-me must be pruned: {report:?}"
    );
    assert!(
        !fw.claude_agents_dir().join("drop-me.md").exists(),
        "pruned agent file removed"
    );
    assert!(
        fw.claude_agents_dir().join("keep-me.md").is_file(),
        "selected agent retained"
    );

    // The pruned agent is gone from the manifest too.
    let manifest = AgentManifest::load(&fw.claude_agents_dir());
    assert!(!manifest.is_managed("drop-me.md"));
    assert!(manifest.is_managed("keep-me.md"));
}

#[test]
fn apply_prune_spares_user_owned() {
    // A user-dropped file (absent from the manifest) must NEVER be pruned, even
    // when its name is not selected.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_agent(&catalog_root, "managed-agent", "BODY");
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    // User drops their own unrelated file into the agents dir.
    let user_file = fw.claude_agents_dir().join("my-own.md");
    fs::write(&user_file, "USER OWNED").unwrap();

    // Apply with prune and a manifest that would NOT select `my-own`.
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();
    assert!(
        !report.agents_pruned.contains(&"my-own.md".to_string()),
        "user-owned file must not be pruned: {report:?}"
    );
    assert!(user_file.is_file(), "user-owned file survives prune");
    assert_eq!(fs::read_to_string(&user_file).unwrap(), "USER OWNED");
}
