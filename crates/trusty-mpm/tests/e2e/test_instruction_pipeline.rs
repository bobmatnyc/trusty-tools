//! E2E: instruction merge pipeline.
//!
//! Exercises `trusty_mpm::core::instruction_pipeline::build_instructions`
//! against temp-directory inputs: a framework `INSTRUCTIONS.md`, a deployed
//! agents directory, and a project `CLAUDE.md`.

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

/// With all three inputs present, the merged output carries every section in
/// the documented framework -> delegation -> project order, and the CLAUDE.md
/// stub is created when absent.
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
    // No CLAUDE.md — the pipeline must seed the stub.
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
    let proj = out
        .merged
        .find("# Project Instructions")
        .expect("project stub section");
    assert!(fw < auth, "framework precedes delegation authority");
    assert!(auth < proj, "delegation authority precedes project notes");
}

/// An existing project `CLAUDE.md` with custom content is preserved verbatim.
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
    assert_eq!(on_disk, custom, "custom content untouched");
    assert!(out.merged.contains("CUSTOM HAND-WRITTEN CONTENT"));
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
