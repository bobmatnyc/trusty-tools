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

/// Seed a DIRECTORY-shaped catalog skill (`<stem>/SKILL.md`) that carries one
/// reference file, so a test can exercise the multi-file ledger keys
/// (`<stem>/references/<file>`) prune has to reason about (#391).
fn seed_catalog_skill_dir(catalog_root: &Path, stem: &str, body: &str, reference: &str) {
    mark_catalog_repo_as_valid(catalog_root);
    let dir = catalog_root.join("repo/.claude/skills").join(stem);
    fs::create_dir_all(dir.join("references")).unwrap();
    fs::write(dir.join("SKILL.md"), format!("# {stem}\n\n{body}\n")).unwrap();
    fs::write(dir.join("references/notes.md"), reference).unwrap();
}

/// Overwrite the project manifest with `body` (the `[agents]`/`[skills]` TOML).
fn rewrite_manifest(project_dir: &Path, body: &str) {
    fs::write(
        project_dir
            .join(".trusty-mpm")
            .join("framework")
            .join("manifest.toml"),
        body,
    )
    .unwrap();
}

/// The single `backup-catalog-prune-*` directory under a framework root, if any.
///
/// Why (#391): the backup root is timestamped, so a test cannot name it up
/// front; it asserts on the one that appeared.
fn prune_backup_dir(fw_root: &Path) -> Option<std::path::PathBuf> {
    fs::read_dir(fw_root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("backup-catalog-prune-"))
        })
}

/// Write a project manifest selecting the CATALOG source for agents and skills.
fn write_catalog_manifest(project_dir: &Path) {
    // #4832: the project manifest layer lives in `.trusty-mpm/framework/`.
    let dir = project_dir.join(".trusty-mpm").join("framework");
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
    assert!(fw.agent_deploy_dir().join("rust-engineer.md").is_file());
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
    assert!(fw.agent_deploy_dir().join("drop-me.md").is_file());

    // Now narrow the manifest to exclude drop-me, and apply WITH prune.
    fs::write(
        project
            .path()
            .join(".trusty-mpm")
            .join("framework")
            .join("manifest.toml"),
        "[agents]\nsource = \"catalog\"\nexclude = [\"drop-me\"]\n\n[skills]\nsource = \"catalog\"\n",
    )
    .unwrap();

    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();
    assert!(
        report.agents_pruned.contains(&"drop-me.md".to_string()),
        "drop-me must be pruned: {report:?}"
    );
    assert!(
        !fw.agent_deploy_dir().join("drop-me.md").exists(),
        "pruned agent file removed"
    );
    assert!(
        fw.agent_deploy_dir().join("keep-me.md").is_file(),
        "selected agent retained"
    );

    // The pruned agent is gone from the manifest too.
    let manifest = AgentManifest::load(&fw.agent_deploy_dir());
    assert!(!manifest.is_managed("drop-me.md"));
    assert!(manifest.is_managed("keep-me.md"));

    // #391: and what it removed is recoverable.
    let backup = prune_backup_dir(&fw.root).expect("a prune that deleted must leave a backup");
    assert!(
        fs::read_to_string(backup.join("agents/drop-me.md"))
            .unwrap()
            .contains("DROP"),
        "the pruned agent is backed up with its original content"
    );
}

#[test]
fn apply_preserves_user_tier_override_deployed_via_launch_path() {
    // Regression (PR #2818 review, CRITICAL 1): `apply_catalog` used to call
    // the single-tier `deploy_skills_filtered` directly against the catalog
    // source, bypassing the user-custom tier entirely. A skill the user tier
    // had previously deployed looked "managed, checksum-matches" to the
    // manifest (which only ever records the LAST writer, not which TIER wrote
    // it), so a differing catalog/bundled body fell into the safe-to-refresh
    // branch and silently clobbered the user's override on every
    // `tm catalog apply`. This proves the fix: deploy the override via the
    // exact multi-tier orchestrator (`deploy_all_skill_tiers`) the session
    // launcher calls, then run `apply_catalog`, and assert the override
    // survives byte-identical.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));
    write_catalog_manifest(project.path());

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    // The catalog (this manifest's selected bundled-equivalent source) ships
    // a DIFFERENT body for the same skill name as the user's override.
    seed_catalog_skill(&catalog_root, "tm-doctor", "CATALOG BODY");

    // The user authors their own override in the user-custom tier.
    let user_source = fw.user_skill_source_dir();
    fs::create_dir_all(&user_source).unwrap();
    fs::write(
        user_source.join("tm-doctor.md"),
        "---\nname: tm-doctor\n---\n\nUSER OVERRIDE BODY\n",
    )
    .unwrap();

    // 1. Deploy via the LAUNCH path: the exact multi-tier orchestrator
    //    `session_launch::prepare_session_inner` calls, seeding the
    //    project's `.claude/skills/` the same way a real session start would.
    let sources = crate::core::manifest::ManifestSources::resolve(project.path(), &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&sources);
    let plan = HarnessPlan::from_manifest(&manifest, &fw, &catalog_root);
    crate::core::skill_tiers::deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &fw.claude_skills_dir(),
        |name| plan.skill_selected(name),
    )
    .unwrap();

    let deployed_path = fw.claude_skills_dir().join("tm-doctor").join("SKILL.md");
    let after_launch = fs::read_to_string(&deployed_path).unwrap();
    assert!(
        after_launch.contains("USER OVERRIDE BODY"),
        "user tier must win at launch: {after_launch}"
    );

    // 2. Run `tm catalog apply` — the exact regression: this must NOT
    //    silently refresh the skill back to catalog content.
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    let after_apply = fs::read_to_string(&deployed_path).unwrap();
    assert_eq!(
        after_apply, after_launch,
        "apply_catalog must preserve the user-tier override byte-identical"
    );
    assert!(
        after_apply.contains("USER OVERRIDE BODY"),
        "override must survive apply: {after_apply}"
    );
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
    let user_file = fw.agent_deploy_dir().join("my-own.md");
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

// ---------------------------------------------------------------------------
// #391 — skill prune had none of the guards every other skill-mutating path has.
// ---------------------------------------------------------------------------

#[test]
fn apply_prune_removes_deselected_skill() {
    // The feature still works: a deselected, PRISTINE, bundled-source skill is
    // removed, directory and ledger entry together. Before #391 this was the
    // only thing skill prune did — and it did it to everything.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_skill(&catalog_root, "keep-me", "KEEP");
    seed_catalog_skill(&catalog_root, "drop-me", "DROP");

    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();
    assert!(fw.claude_skills_dir().join("drop-me/SKILL.md").is_file());

    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\nexclude = [\"drop-me\"]\n",
    );
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert!(
        report.skills_pruned.contains(&"drop-me".to_string()),
        "deselected pristine skill must be pruned: {report:?}"
    );
    assert!(
        !fw.claude_skills_dir().join("drop-me").exists(),
        "pruned skill directory removed"
    );
    assert!(
        fw.claude_skills_dir().join("keep-me/SKILL.md").is_file(),
        "selected skill retained"
    );
    let manifest = SkillManifest::load(&fw.claude_skills_dir());
    assert!(!manifest.is_managed("drop-me"));
    assert!(manifest.is_managed("keep-me"));
}

#[test]
fn apply_prune_skips_hand_edited_skill() {
    // A deselected skill the user HAND-EDITED is frozen, exactly as
    // `deploy_one_file` treats it — prune must not be the one path that deletes
    // what every other path refuses to overwrite.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_skill(&catalog_root, "edited", "ORIGINAL");
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    let deployed = fw.claude_skills_dir().join("edited/SKILL.md");
    fs::write(&deployed, "# edited\n\nMY OWN NOTES\n").unwrap();

    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\nexclude = [\"edited\"]\n",
    );
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        "# edited\n\nMY OWN NOTES\n",
        "a hand-edited skill must survive --prune byte-identical"
    );
    assert!(
        !report.skills_pruned.contains(&"edited".to_string()),
        "hand-edited skill must not be reported pruned: {report:?}"
    );
    // The ledger entry stays too — dropping it would reclassify the file as
    // user-owned and freeze it against every future deploy (#4881's shape).
    assert!(
        SkillManifest::load(&fw.claude_skills_dir()).is_managed("edited"),
        "the kept skill stays managed"
    );
}

#[test]
fn apply_prune_spares_user_tier_skill() {
    // The case a checksum gate alone does NOT catch: a PRISTINE user-custom
    // skill. `deploy_all_skill_tiers` deploys `~/.trusty-mpm/skills/` in full,
    // exempt from the bundled include/exclude — but its ledger entry is then
    // indistinguishable from a bundled one, so a bundled exclude rule used to
    // select it for deletion and its matching checksum waved it through.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_skill(&catalog_root, "bundled-one", "BUNDLED");

    let user_source = fw.user_skill_source_dir();
    fs::create_dir_all(&user_source).unwrap();
    fs::write(user_source.join("mine.md"), "# mine\n\nMY SKILL\n").unwrap();

    // A bundled include list that names only the bundled skill — so the user
    // stem `mine` is "deselected" by the bundled rule it was never subject to.
    write_catalog_manifest(project.path());
    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\ninclude = [\"bundled-one\"]\n",
    );
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    let deployed = fw.claude_skills_dir().join("mine/SKILL.md");
    assert!(
        deployed.is_file(),
        "the user tier deploys regardless of the bundled selection"
    );
    assert!(
        SkillManifest::load(&fw.claude_skills_dir()).is_managed("mine"),
        "and it lands in the ledger looking exactly like a bundled skill"
    );

    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        "# mine\n\nMY SKILL\n",
        "a user-custom skill must survive --prune even when pristine"
    );
    assert!(
        !report.skills_pruned.contains(&"mine".to_string()),
        "user-tier skill must not be pruned: {report:?}"
    );
}

#[test]
fn apply_prune_spares_untracked_file_in_a_managed_skill() {
    // `remove_dir_all` takes the whole directory, so a file the user dropped
    // INSIDE a managed skill disqualifies the skill — the same rule
    // `deploy_one_file` applies to an unmanaged target, extended to the unit
    // prune actually deletes.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_skill(&catalog_root, "hosted", "HOSTED");
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    let stray = fw.claude_skills_dir().join("hosted/my-notes.md");
    fs::write(&stray, "USER NOTES").unwrap();

    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\nexclude = [\"hosted\"]\n",
    );
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert!(stray.is_file(), "the user's own file must survive --prune");
    assert_eq!(fs::read_to_string(&stray).unwrap(), "USER NOTES");
    assert!(
        !report.skills_pruned.contains(&"hosted".to_string()),
        "a skill holding an untracked file must not be pruned: {report:?}"
    );
}

#[test]
fn apply_prune_backs_up_before_removing_a_skill() {
    // A legitimate prune is still recoverable: the directory it removed is
    // copied under a timestamped backup root first, reference files included.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_skill_dir(&catalog_root, "doomed", "DOOMED BODY", "REFERENCE BODY");
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();
    assert!(
        fw.claude_skills_dir()
            .join("doomed/references/notes.md")
            .is_file()
    );

    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\nexclude = [\"doomed\"]\n",
    );
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();
    assert!(report.skills_pruned.contains(&"doomed".to_string()));
    assert!(!fw.claude_skills_dir().join("doomed").exists());

    let backup = prune_backup_dir(&fw.root).expect("a prune that deleted must leave a backup");
    let entry = backup.join("skills/doomed/SKILL.md");
    assert!(entry.is_file(), "backup holds the entry point: {backup:?}");
    assert!(
        fs::read_to_string(&entry).unwrap().contains("DOOMED BODY"),
        "backup holds the ORIGINAL content"
    );
    assert!(
        fs::read_to_string(backup.join("skills/doomed/references/notes.md"))
            .unwrap()
            .contains("REFERENCE BODY"),
        "backup holds the carried reference file too"
    );
}

#[test]
fn apply_prune_keeps_the_ledger_entry_for_a_selected_skills_carried_file() {
    // A ledger key like `<stem>/references/notes.md` names a file the skill
    // CARRIES. Judging keys instead of stems meant an `include` rule matching
    // the stem but not the nested key dropped that file's entry while leaving
    // the file on disk — after which the deployer reads it as user-owned and
    // never updates it again.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_skill_dir(&catalog_root, "carrier", "CARRIER BODY", "REFERENCE BODY");
    write_catalog_manifest(project.path());
    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\ninclude = [\"carrier\"]\n",
    );
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    let key = "carrier/references/notes.md";
    assert!(SkillManifest::load(&fw.claude_skills_dir()).is_managed(key));

    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert!(
        report.skills_pruned.is_empty(),
        "nothing is deselected here: {report:?}"
    );
    assert!(
        fw.claude_skills_dir()
            .join("carrier/references/notes.md")
            .is_file()
    );
    assert!(
        SkillManifest::load(&fw.claude_skills_dir()).is_managed(key),
        "a selected skill's carried file keeps its ledger entry"
    );
}

#[test]
fn apply_prune_skips_hand_edited_agent() {
    // Agent parity: prune deletes a deselected agent only while its bytes still
    // match the ledger. A hand-edited one is the user's work, not stale content.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_agent(&catalog_root, "tweaked", "ORIGINAL");
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    let deployed = fw.agent_deploy_dir().join("tweaked.md");
    fs::write(&deployed, "---\nname: tweaked\n---\n\nMY OWN AGENT\n").unwrap();

    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\nexclude = [\"tweaked\"]\n\n[skills]\nsource = \"catalog\"\n",
    );
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        "---\nname: tweaked\n---\n\nMY OWN AGENT\n",
        "a hand-edited agent must survive --prune byte-identical"
    );
    assert!(
        !report.agents_pruned.contains(&"tweaked.md".to_string()),
        "hand-edited agent must not be reported pruned: {report:?}"
    );
    assert!(AgentManifest::load(&fw.agent_deploy_dir()).is_managed("tweaked.md"));
}

#[test]
fn apply_prune_reports_what_it_kept_and_where_backups_went() {
    // A guard that declines silently is invisible to the operator who asked for
    // a prune. The report names what survived, with the reason, and the backup
    // root for what did not.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_skill(&catalog_root, "pristine", "PRISTINE");
    seed_catalog_skill(&catalog_root, "edited", "ORIGINAL");
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();
    fs::write(
        fw.claude_skills_dir().join("edited/SKILL.md"),
        "MY OWN NOTES",
    )
    .unwrap();

    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\n\n[skills]\nsource = \"catalog\"\nexclude = [\"pristine\", \"edited\"]\n",
    );
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert_eq!(report.skills_pruned, vec!["pristine".to_string()]);
    assert_eq!(report.skills_prune_kept.len(), 1, "{report:?}");
    assert!(
        report.skills_prune_kept[0].starts_with("edited: "),
        "the kept entry names the skill and the reason: {report:?}"
    );
    let backup = report
        .prune_backup_dir
        .as_ref()
        .expect("something was deleted, so a backup root is reported");
    assert!(backup.join("skills/pristine/SKILL.md").is_file());
}

#[test]
fn apply_prune_spares_a_user_origin_agent() {
    // #391 follow-up: the agent ledger records an ownership tier the skill
    // ledger lacks, and `agents::deployer` already honours it — a non-`Bundled`
    // entry is the seed-once tier, preserved on a checksum mismatch. Prune read
    // the checksum and not the tier, so a PRISTINE user-owned agent was deleted
    // by a bundled exclude rule: the exact hole #4979 closed for skills.
    //
    // Nothing writes a non-`Bundled` origin in production today, so this test
    // constructs the ledger entry directly. That is deliberate — the state is
    // unreachable until something does write one, and the point of pinning it
    // now is that the day it becomes reachable it must not also be the day the
    // guard is discovered missing.
    let fw_root = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root.path().join(".trusty-mpm"));

    let catalog_root = crate::content::catalog_root_for(&fw.root);
    seed_catalog_agent(&catalog_root, "operator-owned", "BODY");
    write_catalog_manifest(project.path());
    apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, false).unwrap();

    let deployed = fw.agent_deploy_dir().join("operator-owned.md");
    let before = fs::read_to_string(&deployed).unwrap();

    // Re-attribute the (unmodified) entry to the user tier.
    let target = fw.agent_deploy_dir();
    let mut ledger = AgentManifest::load(&target);
    ledger
        .managed
        .get_mut("operator-owned.md")
        .expect("the deploy recorded it")
        .origin = crate::core::agent_manifest::Origin::User;
    ledger.save(&target).unwrap();

    rewrite_manifest(
        project.path(),
        "[agents]\nsource = \"catalog\"\nexclude = [\"operator-owned\"]\n\n[skills]\nsource = \"catalog\"\n",
    );
    let report = apply_catalog(FakeGitBackend::new(), &fw, project.path(), true, true).unwrap();

    assert_eq!(
        fs::read_to_string(&deployed).unwrap(),
        before,
        "a user-origin agent must survive --prune even while pristine"
    );
    assert!(
        !report
            .agents_pruned
            .contains(&"operator-owned.md".to_string()),
        "user-origin agent must not be pruned: {report:?}"
    );
    assert_eq!(
        report.agents_prune_kept.len(),
        1,
        "and the operator is told it was kept: {report:?}"
    );
    assert!(
        report.agents_prune_kept[0].starts_with("operator-owned.md: "),
        "the kept entry names the agent and the reason: {report:?}"
    );
    // The ledger keeps both the entry and its tier.
    let after = AgentManifest::load(&target);
    assert_eq!(
        after.managed.get("operator-owned.md").map(|e| e.origin),
        Some(crate::core::agent_manifest::Origin::User)
    );
}
