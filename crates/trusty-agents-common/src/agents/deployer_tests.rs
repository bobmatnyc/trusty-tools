//! Tests for `deployer` — split out to mirror the `builder`/`builder_tests`
//! pattern and keep `deployer.rs` under the 500-line SLOC cap.
//!
//! Why: moved verbatim from `trusty-mpm::core::agent_deployer`'s inline
//! `#[cfg(test)] mod tests` (#2892) — behavior-preserving extraction, not a
//! rewrite.
//! What: covers a new deploy, a preserved user-owned file, an unchanged
//! file, the #4408 corrupted-bundled-file repair and its user-owned guard,
//! atomic writes, corrupt-manifest detection, the
//! HR-1 enrichment deploy path, the HR-2 filtered-select predicate, the
//! DOC-42 `declared_skills` population, per-agent compose-failure isolation,
//! and (#4409) `retract_framework_agents` removing only the framework-owned
//! tier from a directory that is no longer a deploy destination.
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

/// The literal degenerate stub from the #4408 incident: a `name:`-only
/// frontmatter with a body of `v1`, no `description:` — 32 bytes that Claude
/// Code still counts as a deployed agent while the roster reports
/// "Agent type not found".
const CORRUPT_STUB: &str = "---\nname: engineer\n---\n\nv1\n";

#[test]
fn deploy_redeploys_corrupted_bundled_file() {
    // #4408 regression: a manifest entry with a FRAMEWORK-owned origin
    // (bundled = the Overwrite tier) whose on-disk content has been replaced
    // by a degenerate stub must be re-deployed from the bundle, not frozen as
    // "user-modified". Before this fix the checksum mismatch was read as a
    // user edit, so the corrupted agent could never be repaired — not by a
    // redeploy, not by `tm validate --repair` — for the life of the workspace.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // First deploy establishes the manifest with origin = Bundled.
    deploy_agents(src.path(), tgt.path()).unwrap();
    let good = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert!(good.contains("Engineer content."));
    assert_eq!(
        AgentManifest::load(tgt.path()).managed["engineer.md"].origin,
        Origin::Bundled
    );

    // Corruption: the deployed copy is replaced by the 32-byte stub while the
    // manifest still records the checksum of the real content.
    fs::write(tgt.path().join("engineer.md"), CORRUPT_STUB).unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(
        result.deployed.contains(&"engineer.md".to_string()),
        "a corrupted bundled agent must be re-deployed, got: {result:?}"
    );
    assert!(!result.skipped.contains(&"engineer.md".to_string()));

    // The real content is back, byte-for-byte, and the manifest agrees with it.
    let repaired = fs::read_to_string(tgt.path().join("engineer.md")).unwrap();
    assert_eq!(repaired, good, "bundled content must be restored exactly");
    assert!(
        AgentManifest::load(tgt.path()).checksum_matches("engineer.md", &repaired),
        "manifest checksum must match the repaired file"
    );

    // Idempotent: a third deploy sees a matching checksum and does nothing.
    let third = deploy_agents(src.path(), tgt.path()).unwrap();
    assert!(third.unchanged.contains(&"engineer.md".to_string()));
    assert!(third.deployed.is_empty());
}

#[test]
fn deploy_preserves_modified_user_owned_entry() {
    // #4408 guard: the user-protection carve-out is intentional for the
    // user-owned (seed-once) tier and must survive the fix — a manifest entry
    // with `Origin::User` whose content was modified is preserved
    // byte-identical across deploy, and reported as skipped.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());

    // Seed a user-owned manifest entry whose on-disk content diverges from
    // both the recorded checksum and the fresh composition.
    let user_content = "---\nname: engineer\n---\n\nUSER HAND-EDIT — KEEP ME\n";
    fs::write(tgt.path().join("engineer.md"), user_content).unwrap();
    let mut manifest = AgentManifest::default();
    manifest.managed.insert(
        "engineer.md".to_string(),
        ManifestEntry {
            source_chain: vec!["engineer".to_string()],
            checksum: checksum("something else entirely"),
            deployed_at: "2026-01-01T00:00:00Z".to_string(),
            origin: Origin::User,
        },
    );
    manifest.save(tgt.path()).unwrap();

    // Two deploys, to prove the preservation is stable and not one-shot.
    for _ in 0..2 {
        let result = deploy_agents(src.path(), tgt.path()).unwrap();
        assert!(
            result.skipped.contains(&"engineer.md".to_string()),
            "user-owned entry must be skipped, got: {result:?}"
        );
        assert!(!result.deployed.contains(&"engineer.md".to_string()));
        assert_eq!(
            fs::read_to_string(tgt.path().join("engineer.md")).unwrap(),
            user_content,
            "user-owned content must survive byte-identical"
        );
    }

    // Its origin is untouched — the deploy never re-registers a skipped file.
    assert_eq!(
        AgentManifest::load(tgt.path()).managed["engineer.md"].origin,
        Origin::User
    );
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
    // this run's write outcome — even a user-owned (skipped) agent's
    // declared skills must still surface for co-deployment purposes.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("code-critic.md"),
        "---\nname: code-critic\nrole: qa\nskills: [code-review-standards]\n---\n\n# Critic\n",
    )
    .unwrap();
    // A user-owned file (untracked by the manifest) sharing the agent's name.
    fs::write(
        tgt.path().join("code-critic.md"),
        "---\nname: code-critic\n---\n\nUSER OWNED\n",
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
fn deploy_refreshes_stale_broken_frontmatter_copy_to_valid_yaml() {
    // Issue #3556 causality check: a deployed copy written by the PRE-FIX
    // composer carries the exact broken shape reported in production — an
    // unquoted `description:` whose value contains ": " — even though the
    // SOURCE template quotes it correctly. Before this fix, recomposing
    // reproduced BYTE-IDENTICAL broken output (`merge_frontmatter` discarded
    // the source's quoting regardless), so `deploy_agents` saw
    // `checksum(composed) == checksum(current)` and reported the file
    // `unchanged` — re-provisioning could never have refreshed it. This test
    // fails against pre-fix code on both counts: (1) the file would stay in
    // `unchanged`, never `deployed`, and (2) even if it were rewritten, the
    // content would still fail strict YAML validation.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("stale-engineer.md"),
        "---\nname: stale-engineer\nrole: engineer\ndescription: 'Rust 2024 edition specialist: memory-safe systems'\n---\n\n# Stale Engineer\n\nBody.\n",
    )
    .unwrap();

    // Simulate a copy deployed by the OLD (broken) composer: unquoted
    // description with a colon, tracked in the manifest as managed and
    // unmodified (its checksum matches exactly what's on disk, exactly as a
    // real prior deploy would have recorded).
    let broken_content = "---\nname: stale-engineer\nrole: engineer\ndescription: Rust 2024 edition specialist: memory-safe systems\n---\n\n# Stale Engineer\n\nBody.\n";
    fs::write(tgt.path().join("stale-engineer.md"), broken_content).unwrap();
    let mut manifest = AgentManifest::default();
    manifest.managed.insert(
        "stale-engineer.md".to_string(),
        ManifestEntry {
            source_chain: vec!["stale-engineer".to_string()],
            checksum: checksum(broken_content),
            deployed_at: "2026-01-01T00:00:00Z".to_string(),
            origin: Origin::Bundled,
        },
    );
    manifest.save(tgt.path()).unwrap();

    // Sanity: the pre-existing deployed copy really is invalid YAML, exactly
    // matching the issue's reported failure mode.
    assert!(
        crate::agents::frontmatter::validate_frontmatter(broken_content).is_err(),
        "the seeded fixture must reproduce genuinely invalid YAML"
    );

    let result = deploy_agents(src.path(), tgt.path()).unwrap();

    assert!(
        result.deployed.contains(&"stale-engineer.md".to_string()),
        "the stale copy must be refreshed (not left `unchanged`), got: {result:?}"
    );
    assert!(!result.unchanged.contains(&"stale-engineer.md".to_string()));

    let refreshed = fs::read_to_string(tgt.path().join("stale-engineer.md")).unwrap();
    crate::agents::frontmatter::validate_frontmatter(&refreshed)
        .unwrap_or_else(|e| panic!("refreshed deployed copy must be valid YAML: {e}\n{refreshed}"));
}

#[test]
fn deploy_isolates_agent_with_invalid_composed_yaml() {
    // Strict-YAML validation (issue #3556) must catch a composition that is
    // still invalid despite the scalar-quoting fix (e.g. a `skills:` list
    // entry starting with a YAML indicator character, which flow-sequence
    // emission does not quote) — logging it, recording it in `failed`, and
    // skipping the write, without aborting the rest of the roster deploy.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    fs::write(
        src.path().join("bad-skills.md"),
        "---\nname: bad-skills\nrole: engineer\nskills:\n  - \"@weird-skill\"\n---\n\n# Bad\n",
    )
    .unwrap();
    fs::write(
        src.path().join("good.md"),
        "---\nname: good\nrole: engineer\n---\n\n# Good\n\nGOOD BODY\n",
    )
    .unwrap();

    let result = deploy_agents(src.path(), tgt.path()).unwrap();

    assert!(result.deployed.contains(&"good.md".to_string()));
    assert!(!tgt.path().join("bad-skills.md").exists());
    assert!(
        result.failed.iter().any(|f| f.starts_with("bad-skills:")),
        "expected an isolated failure entry for bad-skills, got: {:?}",
        result.failed
    );
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

// ---------------------------------------------------------------------------
// Retraction (#4409): the per-workspace tier stops being a deploy destination,
// so the framework-owned copies already sitting there must be removed — and
// nothing else may be.
// ---------------------------------------------------------------------------

/// Deploy `write_sources` into `tgt`, then downgrade one entry to a user-owned
/// origin so retraction has both ownership tiers to discriminate between.
fn deploy_then_mark_user_owned(src: &Path, tgt: &Path, filename: &str) {
    deploy_agents(src, tgt).unwrap();
    let mut manifest = AgentManifest::load(tgt);
    manifest.managed.get_mut(filename).unwrap().origin = Origin::User;
    manifest.save(tgt).unwrap();
}

#[test]
fn retract_removes_framework_owned_only() {
    // The bundled (Overwrite-tier) copy goes; the user-owned (seed-once)
    // entry stays byte-identical, and the manifest keeps claiming it.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());
    deploy_then_mark_user_owned(src.path(), tgt.path(), "base-agent.md");
    let user_owned_before = fs::read_to_string(tgt.path().join("base-agent.md")).unwrap();

    let result = retract_framework_agents(tgt.path()).unwrap();

    assert_eq!(result.removed, vec!["engineer.md".to_string()]);
    assert_eq!(result.preserved, vec!["base-agent.md".to_string()]);
    assert!(
        !tgt.path().join("engineer.md").exists(),
        "the framework-owned copy must be gone"
    );
    assert_eq!(
        fs::read_to_string(tgt.path().join("base-agent.md")).unwrap(),
        user_owned_before,
        "a user-owned entry must survive byte-identical"
    );

    let manifest = AgentManifest::load(tgt.path());
    assert!(!manifest.is_managed("engineer.md"));
    assert!(manifest.is_managed("base-agent.md"));
}

#[test]
fn retract_preserves_untracked_hand_placed_file() {
    // A file the operator dropped in by hand is absent from the manifest and
    // must be invisible to retraction.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());
    deploy_agents(src.path(), tgt.path()).unwrap();

    let hand_placed = tgt.path().join("my-own-agent.md");
    let content = "---\nname: my-own-agent\ndescription: mine\n---\n\nMine.\n";
    fs::write(&hand_placed, content).unwrap();

    let result = retract_framework_agents(tgt.path()).unwrap();

    assert!(!result.removed.contains(&"my-own-agent.md".to_string()));
    assert!(!result.preserved.contains(&"my-own-agent.md".to_string()));
    assert_eq!(fs::read_to_string(&hand_placed).unwrap(), content);
}

#[test]
fn retract_removes_drifted_framework_file() {
    // Checksum drift on a framework-owned file is corruption, not ownership
    // (#4408's ruling). Retraction must not freeze the #4408 stub in place —
    // that is precisely the shadowing copy #4409 exists to clear.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());
    deploy_agents(src.path(), tgt.path()).unwrap();
    fs::write(tgt.path().join("engineer.md"), CORRUPT_STUB).unwrap();

    let result = retract_framework_agents(tgt.path()).unwrap();

    assert!(result.removed.contains(&"engineer.md".to_string()));
    assert!(!tgt.path().join("engineer.md").exists());
}

#[test]
fn retract_clears_manifest_and_dir_when_nothing_remains() {
    // A workspace whose agents were ALL framework-owned returns to pristine:
    // no ledger left behind, no empty directory.
    let src = TempDir::new().unwrap();
    let base = TempDir::new().unwrap();
    let tgt = base.path().join("agents");
    write_sources(src.path());
    deploy_agents(src.path(), &tgt).unwrap();

    let result = retract_framework_agents(&tgt).unwrap();

    assert_eq!(result.removed.len(), 2);
    assert!(!tgt.exists(), "an emptied agents dir must be removed");
}

#[test]
fn retract_is_idempotent() {
    // The flip runs retraction on every session prepare; the second run must
    // be a clean no-op rather than an error.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());
    deploy_agents(src.path(), tgt.path()).unwrap();

    let first = retract_framework_agents(tgt.path()).unwrap();
    assert_eq!(first.removed.len(), 2);

    let second = retract_framework_agents(tgt.path()).unwrap();
    assert!(second.removed.is_empty());
    assert!(second.preserved.is_empty());
}

#[test]
fn retract_missing_dir_is_a_noop() {
    // A workspace that never received a deploy has nothing to retract.
    let tmp = TempDir::new().unwrap();
    let result = retract_framework_agents(&tmp.path().join("never-deployed")).unwrap();
    assert_eq!(result, RetractResult::default());
}

#[test]
fn retract_filtered_respects_predicate() {
    // #4409: `tm install --reset-agents <names> --reset-agents-workspaces`
    // names a scope, and the workspace half of that sweep is a retraction. An
    // entry the predicate rejects must stay BOTH on disk and in the ledger —
    // dropping it from the ledger while leaving the file would reclassify it as
    // untracked on the next run.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());
    deploy_agents(src.path(), tgt.path()).unwrap();

    let result = retract_framework_agents_filtered(tgt.path(), |stem| stem == "engineer").unwrap();

    assert_eq!(result.removed, vec!["engineer.md".to_string()]);
    assert!(!tgt.path().join("engineer.md").exists());
    assert!(
        tgt.path().join("base-agent.md").exists(),
        "an agent outside the requested scope must survive"
    );
    assert!(
        AgentManifest::load(tgt.path()).is_managed("base-agent.md"),
        "a skipped entry must stay tracked, or the next deploy sees it as untracked"
    );
}

#[test]
fn retract_refuses_on_corrupt_manifest() {
    // Deleting files on the strength of an unreadable ownership ledger is the
    // exact failure mode the manifest exists to prevent.
    let src = TempDir::new().unwrap();
    let tgt = TempDir::new().unwrap();
    write_sources(src.path());
    deploy_agents(src.path(), tgt.path()).unwrap();
    fs::write(tgt.path().join(MANIFEST_FILE), b"not valid json{{{").unwrap();

    let err = retract_framework_agents(tgt.path()).unwrap_err();
    assert!(
        err.to_string().contains("corrupt"),
        "error must name the corruption, got: {err}"
    );
    assert!(
        tgt.path().join("engineer.md").exists(),
        "no file may be removed when the ledger is unreadable"
    );
}
