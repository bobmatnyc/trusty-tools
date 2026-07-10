//! E2E: instruction merge pipeline.
//!
//! Exercises `trusty_mpm::core::instruction_pipeline::build_instructions`
//! against temp-directory inputs: a framework `INSTRUCTIONS.md`, a deployed
//! agents directory, and a project `CLAUDE.md` (the latter is only seeded as
//! a side effect — its content is not folded into `merged`; see the crate's
//! `instruction_pipeline` module docs for why that concatenation was removed
//! as dead code).

use std::path::PathBuf;

use crate::harness::write_agent_sources;
use tempfile::TempDir;
use trusty_mpm::core::agent_deployer::deploy_agents;
use trusty_mpm::core::instruction_pipeline::{PipelineInput, build_instructions};

/// Build a `PipelineInput` rooted at `tmp`.
fn input_in(tmp: &TempDir) -> PipelineInput {
    PipelineInput {
        framework_instructions_path: tmp.path().join("INSTRUCTIONS.md"),
        agents_dir: tmp.path().join("agents"),
        claude_md_path: tmp.path().join("project").join("CLAUDE.md"),
    }
}

fn write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, content).expect("write file");
}

/// With framework instructions and deployed agents present, the merged output
/// carries both sections in the documented framework -> delegation order, and
/// the CLAUDE.md stub is still created (as a side effect) when absent — but
/// its content is NOT folded into `merged` (dead code removed: Claude Code
/// loads CLAUDE.md natively, and the real launch prompt never read this
/// field for that content).
#[test]
fn pipeline_produces_merged_output() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);

    write(
        &input.framework_instructions_path,
        "# Framework\n\nFRAMEWORK SECTION\n",
    );
    std::fs::create_dir_all(&input.agents_dir).unwrap();
    write(
        &input.agents_dir.join("engineer.md"),
        "---\nname: engineer\nrole: engineer\ndescription: Builds things.\n---\n\n# Engineer\n",
    );
    // No CLAUDE.md — the pipeline must still seed the stub.
    assert!(!input.claude_md_path.exists());

    let out = build_instructions(&input).expect("pipeline succeeds");
    assert!(out.instructions_loaded);
    assert!(out.claude_md_created, "stub created when CLAUDE.md absent");
    assert!(input.claude_md_path.exists(), "stub written to disk");

    let fw = out.merged.find("FRAMEWORK SECTION").expect("framework");
    let auth = out
        .merged
        .find("## Delegation Authority")
        .expect("delegation section");
    assert!(fw < auth, "framework precedes delegation authority");
    assert!(
        !out.merged.contains("# Project Instructions"),
        "project CLAUDE.md stub content must not be folded into merged: {}",
        out.merged
    );
}

/// An existing project `CLAUDE.md` with custom content is left completely
/// untouched on disk (issue #2170 — trusty-mpm must never modify a target
/// project's `CLAUDE.md`; the delegation directive is delivered exclusively
/// via the `trusty-mpm` output style), and its content does not leak into
/// `merged` (dead code removed).
#[test]
fn pipeline_preserves_claude_md() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);

    write(&input.framework_instructions_path, "# Framework\n");
    std::fs::create_dir_all(&input.agents_dir).unwrap();
    let custom = "# My Project\n\nCUSTOM HAND-WRITTEN CONTENT\n";
    write(&input.claude_md_path, custom);

    let out = build_instructions(&input).expect("pipeline succeeds");
    assert!(!out.claude_md_created, "existing CLAUDE.md not recreated");

    let on_disk = std::fs::read_to_string(&input.claude_md_path).unwrap();
    assert_eq!(
        on_disk, custom,
        "CLAUDE.md must be left byte-identical: {on_disk}"
    );
    assert!(
        !on_disk.contains("trusty-mpm:delegation-directive:begin"),
        "no delegation-directive block may be injected: {on_disk}"
    );
    assert!(
        !out.merged.contains("CUSTOM HAND-WRITTEN CONTENT"),
        "project CLAUDE.md content must not be folded into merged: {}",
        out.merged
    );
}

/// A missing `INSTRUCTIONS.md` is not fatal — the pipeline still succeeds and
/// reports `instructions_loaded = false`.
#[test]
fn pipeline_without_instructions_file() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);
    // No INSTRUCTIONS.md written.
    std::fs::create_dir_all(&input.agents_dir).unwrap();

    let out = build_instructions(&input).expect("pipeline succeeds");
    assert!(!out.instructions_loaded);
    assert!(!out.merged.starts_with("---"), "no dangling separator");
}

/// A deployed `engineer.md` agent surfaces in the merged delegation authority
/// section. This wires the deploy step and the pipeline together end to end.
#[test]
fn pipeline_authority_lists_deployed_agents() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);
    write(&input.framework_instructions_path, "# Framework\n");

    // Deploy a real composed engineer agent into the pipeline's agents dir.
    let src = TempDir::new().unwrap();
    write_agent_sources(src.path());
    deploy_agents(src.path(), &input.agents_dir).expect("deploy succeeds");

    let out = build_instructions(&input).expect("pipeline succeeds");
    assert!(out.agent_count >= 1, "deployed agents counted");
    assert!(
        out.merged.contains("## Delegation Authority"),
        "delegation section present"
    );
    assert!(
        out.merged.contains("engineer"),
        "deployed engineer listed in delegation authority"
    );
}
