//! Tests for `deployer` — split out to mirror the `builder`/`builder_tests`
//! pattern and keep `deployer.rs` under the 500-line SLOC cap.
//!
//! Why: moved verbatim from `trusty-mpm::core::agent_deployer`'s inline
//! `#[cfg(test)] mod tests` (#2892) — behavior-preserving extraction, not a
//! rewrite.
//! What: covers a new deploy, a skipped user-modified file, an unchanged
//! file, a user-owned file, atomic writes, corrupt-manifest detection, the
//! HR-1 enrichment deploy path, the HR-2 filtered-select predicate, the
//! DOC-42 `declared_skills` population, and per-agent compose-failure
//! isolation.
//! Test: this file IS the test module for `deployer`; run with
//! `cargo test -p trusty-agents-common -- agents::deployer`.

use super::*;
use std::fs;
use tempfile::TempDir;

/// A two-file source set: a base agent and a leaf that extends it.
fn write_sources(dir: &Path) {
    fs::write(
        dir.join("base-agent.md"),
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBase content.\n",
    )
    .unwrap();
    fs::write(
        dir.join("engineer.md"),
        "---\nname: engineer\nrole: engineer\nextends: base-agent\nmodel: sonnet\n---\n\n# Engineer\n\nEngineer content.\n",
    )
    .unwrap();
}

#[test]
fn deploy_new_agent() {
    // A first-ever deploy must write every composed agent and record it
    // in the manifest.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert_eq!(result.deployed.len(), 2);
    assert!(result.deployed.contains(&"engineer.md".to_string()));
    assert!(result.skipped.is_empty());
    assert!(result.unchanged.is_empty());

    // Files exist and the composed engineer carries inherited content.
    let engineer = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert!(engineer.contains("Base content."));
    assert!(engineer.contains("Engineer content."));

    // The manifest records the resolved chain.
    let manifest = AgentManifest::load(tgt.path());
    assert!(manifest.is_managed("engineer.md"));
    assert_eq!(
        manifest.managed["engineer.md"].source_chain,
        vec!["base-agent", "engineer"]
    );
}

#[test]
fn deploy_skips_user_modified() {
    // A managed file the user edited must be skipped, not overwritten.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // First deploy establishes the manifest.
    deploy_agents(src.path(), tgt.path()).unwrap();

    // User edits the deployed engineer file.
    fs::write(
        tgt.path().join("engineer.md"),
        "---\nname: engineer\n---\n\nUSER HAND-EDIT\n",
    )
    .unwrap();

    // Second deploy must preserve the user's edit.
    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(result.skipped.contains(&"engineer.md".to_string()));
    assert!(!result.deployed.contains(&"engineer.md".to_string()));

    let still = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert!(still.contains("USER HAND-EDIT"));
}

#[test]
fn deploy_adopts_untracked_byte_identical_file() {
    // Issue #2504: a target file present on disk but absent from the
    // manifest (predates per-file tracking) must be silently adopted
    // when its content already equals the fresh composition — no
    // rewrite, just registration — so a later bundle change can reach it.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // Pre-create the target with exactly what compose_agent would
    // produce, simulating a file deployed before manifest tracking
    // existed for it.
    let composed = crate::agents::builder::compose_agent("engineer", src.path()).unwrap();
    fs::write(tgt.path().join("engineer.md"), &composed).unwrap();
    let before = fs::metadata(tgt.path().join("engineer.md"))
        .unwrap()
        .modified()
        .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(result.adopted.contains(&"engineer.md".to_string()));
    assert!(!result.skipped.contains(&"engineer.md".to_string()));
    assert!(!result.deployed.contains(&"engineer.md".to_string()));

    // Not rewritten — adoption is registration-only.
    let after = fs::metadata(tgt.path().join("engineer.md"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "adopted file must not be rewritten");

    // Now registered — a subsequent bundle change must be able to reach
    // it (the whole point of adoption).
    let manifest = AgentManifest::load(tgt.path());
    assert!(manifest.is_managed("engineer.md"));

    fs::write(
        src.path().join("engineer.md"),
        "---\nname: engineer\nrole: engineer\nextends: base-agent\nmodel: sonnet\n---\n\n# Engineer\n\nUPDATED.\n",
    )
    .unwrap();
    let second = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(second.deployed.contains(&"engineer.md".to_string()));
    let updated = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert!(updated.contains("UPDATED."));
}

#[test]
fn deploy_flags_untracked_modified_file_for_reset() {
    // An untracked file whose content differs from the fresh composition
    // must be skipped AND recorded in `untracked_modified` so the CLI can
    // point the operator at `tm install --reset-agents`.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());
    fs::write(
        tgt.path().join("engineer.md"),
        "PRE-EXISTING, NOT TRUSTY-MPM'S CURRENT COMPOSITION\n",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(result.skipped.contains(&"engineer.md".to_string()));
    assert!(
        result
            .untracked_modified
            .contains(&"engineer.md".to_string())
    );
    assert!(!AgentManifest::load(tgt.path()).is_managed("engineer.md"));
}

#[test]
fn deploy_unchanged_no_write() {
    // A second deploy with no source changes must report files unchanged
    // and not rewrite them.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_agents(src.path(), tgt.path()).unwrap();
    let before = fs::metadata(tgt.path().join("engineer.md"))
        .unwrap()
        .modified()
        .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(result.unchanged.contains(&"engineer.md".to_string()));
    assert!(result.deployed.is_empty());

    let after = fs::metadata(tgt.path().join("engineer.md"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "unchanged file must not be rewritten");
}

#[test]
fn deploy_user_owned_skipped() {
    // A file in the target that trusty-mpm never deployed (absent from the
    // manifest) must be left completely untouched.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // User pre-creates a file matching a source agent's name.
    fs::write(
        tgt.path().join("engineer.md"),
        "USER OWNED — not trusty-mpm's\n",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(result.skipped.contains(&"engineer.md".to_string()));

    // The user's content survives untouched.
    let content = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert_eq!(content, "USER OWNED — not trusty-mpm's\n");

    // base-agent.md had no conflict, so it deploys normally.
    assert!(result.deployed.contains(&"base-agent.md".to_string()));
}

#[test]
fn deploy_missing_source_dir_is_empty_result() {
    // Deploying from a non-existent source directory is a no-op success.
    let tgt = TempDir::new().unwrap();
    let result = deploy_agents(Path::new("/nonexistent/trusty-mpm/agents"), tgt.path()).unwrap();
    assert_eq!(result, DeployResult::default());
}

#[test]
fn deploy_aborts_on_corrupt_manifest() {
    // A corrupt manifest file must cause deploy_agents to return an error
    // instead of silently resetting to empty and reclassifying managed
    // files as user-owned.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // Write a malformed manifest to the target directory.
    fs::write(
        tgt.path().join(crate::agents::manifest::MANIFEST_FILE),
        b"not valid json{{{",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path());
    assert!(
        result.is_err(),
        "corrupt manifest must cause an error, not a silent reset to empty"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("corrupt") || err_msg.contains("repair"),
        "error message must mention corruption and repair: {err_msg}"
    );
}

#[test]
fn deploy_injects_initial_prompt_and_tier_model() {
    // A deployed agent that declares a `resource_tier` but no `model`, and
    // an engineer `role` but no `initialPrompt`, must land on disk with both
    // deploy-time enrichments applied (HR-1).
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("base-engineer.md"),
        "---\nname: base-engineer\nrole: base-engineer\n---\n\n# Base Eng\n\nBASE ENG CONTENT\n",
    )
    .unwrap();
    fs::write(
        src.path().join("heavy-eng.md"),
        "---\nname: heavy-eng\nrole: engineer\nextends: base-engineer\nresource_tier: intensive\n---\n\n# Heavy\n\nLEAF CONTENT\n",
    )
    .unwrap();

    deploy_agents(src.path(), tgt.path()).unwrap();

    let deployed = fs::read_to_string(tgt.path().join("heavy-eng.md")).unwrap();
    assert!(
        deployed.contains("model: opus"),
        "intensive tier must inject opus on deploy; got:\n{deployed}"
    );
    assert!(
        deployed.contains(r#"initialPrompt: "Begin implementation."#),
        "engineer role must inject implementation initialPrompt on deploy; got:\n{deployed}"
    );
    // Inherited base content survives composition + deploy.
    assert!(deployed.contains("BASE ENG CONTENT"));
    assert!(deployed.contains("LEAF CONTENT"));
}

#[test]
fn deploy_preserves_explicit_model_and_prompt() {
    // Explicit `model` and explicit `initialPrompt` in the source must
    // survive deploy unchanged (explicit always wins over enrichment).
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("pinned.md"),
        "---\nname: pinned\nrole: engineer\nresource_tier: intensive\nmodel: haiku\ninitialPrompt: Custom start.\n---\n\n# Pinned\n",
    )
    .unwrap();

    deploy_agents(src.path(), tgt.path()).unwrap();

    let deployed = fs::read_to_string(tgt.path().join("pinned.md")).unwrap();
    assert!(
        deployed.contains("model: haiku"),
        "explicit model wins:\n{deployed}"
    );
    assert!(!deployed.contains("model: opus"));
    assert!(
        deployed.contains(r#"initialPrompt: "Custom start.""#),
        "explicit prompt wins:\n{deployed}"
    );
    assert!(!deployed.contains("Begin implementation."));
}

#[test]
fn deploy_filtered_respects_predicate() {
    // HR-2: a selection predicate must restrict which source agents deploy.
    // Only the accepted agent lands; the rejected one is never written.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // base-agent.md + engineer.md

    let result = deploy_agents_filtered(src.path(), tgt.path(), |name| name == "engineer").unwrap();

    // engineer.md deployed; base-agent.md filtered out and not written.
    assert!(result.deployed.contains(&"engineer.md".to_string()));
    assert!(!result.deployed.contains(&"base-agent.md".to_string()));
    assert!(tgt.path().join("engineer.md").exists());
    assert!(!tgt.path().join("base-agent.md").exists());
}

#[test]
fn declared_skills_populated_for_every_processed_agent() {
    // DOC-42: an agent declaring `skills:` must have that list recorded
    // in `declared_skills`, keyed by agent name (stem).
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("code-critic.md"),
        "---\nname: code-critic\nrole: qa\nskills: [code-review-standards, systematic-debugging]\n---\n\n# Critic\n",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert_eq!(
        result.declared_skills.get("code-critic"),
        Some(&vec![
            "code-review-standards".to_string(),
            "systematic-debugging".to_string()
        ])
    );
}

#[test]
fn declared_skills_empty_when_agent_declares_none() {
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path()); // base-agent.md + engineer.md, neither declares skills

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert_eq!(result.declared_skills.get("engineer"), Some(&Vec::new()));
}

#[test]
fn declared_skills_populated_even_when_deploy_is_skipped() {
    // The declaration is a property of the SOURCE composition, not of
    // this run's write outcome — even a user-modified (skipped) agent's
    // declared skills must still surface for co-deployment purposes.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("code-critic.md"),
        "---\nname: code-critic\nrole: qa\nskills: [code-review-standards]\n---\n\n# Critic\n",
    )
    .unwrap();
    deploy_agents(src.path(), tgt.path()).unwrap();
    fs::write(
        tgt.path().join("code-critic.md"),
        "---\nname: code-critic\n---\n\nUSER HAND-EDIT\n",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(result.skipped.contains(&"code-critic.md".to_string()));
    assert_eq!(
        result.declared_skills.get("code-critic"),
        Some(&vec!["code-review-standards".to_string()])
    );
}

#[test]
fn deploy_isolates_single_malformed_agent_failure() {
    // Issue #2906 review (CRITICAL finding): one malformed agent asset
    // (here, unterminated frontmatter — missing closing `---`) must NOT
    // abort the entire roster deploy. The well-formed sibling agent must
    // still deploy, and the failure must be recorded (not silently
    // dropped) rather than propagated as an `Err` from `deploy_agents`.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("broken.md"),
        "---\nname: broken\n\n# No closing fence\n",
    )
    .unwrap();
    fs::write(
        src.path().join("good.md"),
        "---\nname: good\nrole: engineer\n---\n\n# Good\n\nGOOD BODY\n",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path())
        .expect("a single malformed agent must not abort the whole deploy");

    assert!(result.deployed.contains(&"good.md".to_string()));
    assert!(tgt.path().join("good.md").is_file());
    assert!(!tgt.path().join("broken.md").exists());
    assert_eq!(result.failed.len(), 1);
    assert!(result.failed[0].starts_with("broken:"));
}

#[test]
fn deploy_content_file_is_atomic() {
    // After a successful deploy no stale .tmp file should remain in the
    // target directory — the atomic rename must have completed.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    deploy_agents(src.path(), tgt.path()).unwrap();

    for entry in fs::read_dir(tgt.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        assert!(
            !name_str.ends_with(".tmp"),
            "stale .tmp file found after deploy: {name_str}"
        );
    }
}
