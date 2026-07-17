//! Tests for `deployer` — split out to mirror the `builder`/`builder_tests`
//! pattern and keep `deployer.rs` under the 500-line SLOC cap.
//!
//! Why: moved verbatim from `trusty-mpm::core::skill_deployer`'s inline
//! `#[cfg(test)] mod tests` (#2892, #2818), plus the reference-file mirroring
//! tests ported from `trusty-mpm` PR #2915 (issue #2903) — behavior-preserving
//! extraction, not a rewrite.
//! What: covers a new deploy, a skipped user-modified file, an unchanged
//! file, a user-owned file, the `mcp`-name collision guard, and the
//! `references/*.md` sibling-file mirroring cases.
//! Test: this file IS the test module for `deployer`; run with
//! `cargo test -p trusty-agents-common -- skills::deployer`.

use super::*;
use std::fs;
use tempfile::TempDir;

/// A two-file skill source set.
fn write_sources(dir: &Path) {
    fs::write(
        dir.join("tm-doctor.md"),
        "---\nname: tm-doctor\n---\n\n# Doctor\n\nDiagnostic skill.\n",
    )
    .unwrap();
    fs::write(
        dir.join("example-skill.md"),
        "---\nname: example-skill\n---\n\n# Example\n\nExample skill.\n",
    )
    .unwrap();
}

#[test]
fn deploy_new_skill() {
    // A first-ever deploy must write every skill as <dest>/<name>/SKILL.md
    // and record the skill name (stem) in the manifest.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert_eq!(stats.deployed.len(), 2);
    // Stats report stems, not filenames.
    assert!(stats.deployed.contains(&"tm-doctor".to_string()));
    assert!(stats.skipped.is_empty());
    assert!(stats.unchanged.is_empty());

    // Each skill lands at <dest>/<name>/SKILL.md — not a flat .md file.
    let doctor = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
    assert!(doctor.contains("Diagnostic skill."));

    let manifest = SkillManifest::load(tgt.path());
    assert!(manifest.is_managed("tm-doctor"));
}

#[test]
fn deploy_filtered_respects_predicate() {
    // HR-2: a selection predicate restricts which source skills deploy.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md

    let stats =
        deploy_skills_filtered(src.path(), tgt.path(), |name| name == "example-skill").unwrap();

    assert!(stats.deployed.contains(&"example-skill".to_string()));
    assert!(!stats.deployed.contains(&"tm-doctor".to_string()));
    assert!(tgt.path().join("example-skill").join("SKILL.md").exists());
    assert!(!tgt.path().join("tm-doctor").exists());
}

#[test]
fn deploy_skips_user_modified() {
    // A managed file the user edited must be skipped, not overwritten.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_skills(src.path(), tgt.path()).unwrap();

    // Simulate the user editing the deployed SKILL.md.
    fs::write(
        tgt.path().join("tm-doctor").join("SKILL.md"),
        "---\nname: tm-doctor\n---\n\nUSER HAND-EDIT\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.skipped.contains(&"tm-doctor".to_string()));
    assert!(!stats.deployed.contains(&"tm-doctor".to_string()));

    let still = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
    assert!(still.contains("USER HAND-EDIT"));
}

#[test]
fn deploy_unchanged_no_write() {
    // A second deploy with no source changes must report files unchanged
    // and not rewrite them.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_skills(src.path(), tgt.path()).unwrap();
    let skill_md = tgt.path().join("tm-doctor").join("SKILL.md");
    let before = fs::metadata(&skill_md).unwrap().modified().unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.unchanged.contains(&"tm-doctor".to_string()));
    assert!(stats.deployed.is_empty());

    let after = fs::metadata(&skill_md).unwrap().modified().unwrap();
    assert_eq!(before, after, "unchanged file must not be rewritten");
}

#[test]
fn deploy_user_owned_skipped() {
    // A SKILL.md in the target that trusty-mpm never deployed (absent from
    // the manifest) must be left completely untouched.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // Pre-create a user-owned skill directory for tm-doctor.
    let user_dir = tgt.path().join("tm-doctor");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(user_dir.join("SKILL.md"), "USER OWNED — not trusty-mpm's\n").unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.skipped.contains(&"tm-doctor".to_string()));

    let content = fs::read_to_string(user_dir.join("SKILL.md")).unwrap();
    assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

    // example-skill had no conflict, so it deploys normally.
    assert!(stats.deployed.contains(&"example-skill".to_string()));
}

#[test]
fn deploy_refreshes_stale_managed_skill() {
    // A managed, unmodified file whose source changed must be refreshed.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_skills(src.path(), tgt.path()).unwrap();

    // The framework updates the source skill.
    fs::write(
        src.path().join("tm-doctor.md"),
        "---\nname: tm-doctor\n---\n\n# Doctor v2\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(stats.deployed.contains(&"tm-doctor".to_string()));
    let refreshed = fs::read_to_string(tgt.path().join("tm-doctor").join("SKILL.md")).unwrap();
    assert!(refreshed.contains("Doctor v2"));
}

#[test]
fn deploy_skips_mcp_named_skill() {
    // #2186: a skill whose name (stem) contains "mcp" must never be
    // deployed — Claude Code's built-in `/mcp` command would be shadowed
    // by `/toolchains-ai-protocols-mcp` (or any other mcp-containing
    // name). It must be recorded as skipped, not deployed, and must not
    // block sibling skills in the same batch from deploying normally.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md
    fs::write(
        src.path().join("toolchains-ai-protocols-mcp.md"),
        "---\nname: toolchains-ai-protocols-mcp\n---\n\n# AI Protocols\n\nMCP guidance.\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(
        stats
            .skipped
            .contains(&"toolchains-ai-protocols-mcp".to_string())
    );
    assert!(
        !stats
            .deployed
            .contains(&"toolchains-ai-protocols-mcp".to_string())
    );
    assert!(
        !tgt.path()
            .join("toolchains-ai-protocols-mcp")
            .join("SKILL.md")
            .exists(),
        "an mcp-named skill must never be written to disk"
    );

    // Sibling non-mcp skills in the same batch must still deploy.
    assert!(stats.deployed.contains(&"tm-doctor".to_string()));
    assert!(stats.deployed.contains(&"example-skill".to_string()));
}

#[test]
fn deploy_skips_mcp_named_skill_case_insensitive() {
    // The guard must match "mcp" regardless of case (e.g. `Foo-MCP.md`).
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md
    fs::write(
        src.path().join("Foo-MCP.md"),
        "---\nname: Foo-MCP\n---\n\n# Foo MCP\n\nSomething.\n",
    )
    .unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(stats.skipped.contains(&"Foo-MCP".to_string()));
    assert!(!stats.deployed.contains(&"Foo-MCP".to_string()));
    assert!(!tgt.path().join("Foo-MCP").exists());
}

#[test]
fn deploy_missing_source_dir_is_empty_result() {
    // Deploying from a non-existent source directory is a no-op success.
    let tgt = TempDir::new().unwrap();
    let stats = deploy_skills(Path::new("/nonexistent/trusty-mpm/skills"), tgt.path()).unwrap();
    assert_eq!(stats, DeployStats::default());
}

/// Write a multi-file skill: `<dir>/<stem>.md` plus `<dir>/<stem>/references/*.md`.
fn write_multi_file_skill(dir: &Path, stem: &str, refs: &[(&str, &str)]) {
    fs::write(
        dir.join(format!("{stem}.md")),
        format!("---\nname: {stem}\n---\n\nEntry point.\n"),
    )
    .unwrap();
    let refs_dir = dir.join(stem).join("references");
    fs::create_dir_all(&refs_dir).unwrap();
    for (name, body) in refs {
        fs::write(refs_dir.join(name), body).unwrap();
    }
}

#[test]
fn deploy_reference_files_land_alongside_skill() {
    // Issue #2903: a multi-file skill's references/*.md siblings must
    // deploy to <dest>/<stem>/references/<file> alongside SKILL.md.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_multi_file_skill(
        src.path(),
        "systematic-debugging",
        &[("workflow.md", "WORKFLOW"), ("examples.md", "EXAMPLES")],
    );

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(stats.deployed.contains(&"systematic-debugging".to_string()));
    assert!(
        stats
            .deployed
            .contains(&"systematic-debugging/references/workflow.md".to_string())
    );
    assert!(
        stats
            .deployed
            .contains(&"systematic-debugging/references/examples.md".to_string())
    );

    let workflow = fs::read_to_string(
        tgt.path()
            .join("systematic-debugging")
            .join("references")
            .join("workflow.md"),
    )
    .unwrap();
    assert_eq!(workflow, "WORKFLOW");
    let examples = fs::read_to_string(
        tgt.path()
            .join("systematic-debugging")
            .join("references")
            .join("examples.md"),
    )
    .unwrap();
    assert_eq!(examples, "EXAMPLES");
}

#[test]
fn deploy_reference_files_skip_user_modified() {
    // A user-edited reference file must be preserved, not overwritten,
    // exactly like a user-edited SKILL.md.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_multi_file_skill(src.path(), "systematic-debugging", &[("workflow.md", "V1")]);
    deploy_skills(src.path(), tgt.path()).unwrap();

    let ref_path = tgt
        .path()
        .join("systematic-debugging")
        .join("references")
        .join("workflow.md");
    fs::write(&ref_path, "USER HAND-EDIT").unwrap();

    // Source changes; user's edit must survive the redeploy.
    fs::write(
        src.path()
            .join("systematic-debugging")
            .join("references")
            .join("workflow.md"),
        "V2",
    )
    .unwrap();
    let stats = deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(
        stats
            .skipped
            .contains(&"systematic-debugging/references/workflow.md".to_string())
    );
    assert_eq!(fs::read_to_string(&ref_path).unwrap(), "USER HAND-EDIT");
}

#[test]
fn deploy_reference_files_sync_even_when_entry_unchanged() {
    // A new reference file added to a skill whose SKILL.md content did
    // NOT change must still deploy — reference sync is independent of
    // whether the entry point itself changed this pass.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_multi_file_skill(src.path(), "systematic-debugging", &[]);
    deploy_skills(src.path(), tgt.path()).unwrap();

    // SKILL.md is untouched, but a reference file is added afterward.
    let refs_dir = src.path().join("systematic-debugging").join("references");
    fs::write(refs_dir.join("new-ref.md"), "NEW REF").unwrap();

    let stats = deploy_skills(src.path(), tgt.path()).unwrap();
    assert!(
        stats
            .unchanged
            .contains(&"systematic-debugging".to_string()),
        "entry point content is unchanged: {stats:?}"
    );
    assert!(
        stats
            .deployed
            .contains(&"systematic-debugging/references/new-ref.md".to_string())
    );
    assert!(
        tgt.path()
            .join("systematic-debugging")
            .join("references")
            .join("new-ref.md")
            .is_file()
    );
}

#[test]
fn deploy_single_file_skill_has_no_references_dir() {
    // A skill with no references/ subtree must not create an empty
    // references/ directory under the deployed skill.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // tm-doctor.md + example-skill.md, no refs

    deploy_skills(src.path(), tgt.path()).unwrap();

    assert!(!tgt.path().join("tm-doctor").join("references").exists());
}
