//! E2E: instruction pipeline.
//!
//! Exercises `trusty_mpm::core::instruction_pipeline::build_instructions`
//! against temp-directory inputs: a deployed agents directory and a project
//! `CLAUDE.md`.
//!
//! #4832 removed the pipeline's `merged` text output and the framework
//! `INSTRUCTIONS.md` read that fed it, so these tests now assert the two things
//! the pipeline still contracts for: the roster COUNT it reports (which
//! `tm session start` prints verbatim) and the `CLAUDE.md` stub side effect.

use std::path::PathBuf;

use crate::harness::write_agent_sources;
use tempfile::TempDir;
use trusty_mpm::core::agent_deployer::deploy_agents;
use trusty_mpm::core::instruction_pipeline::{PipelineInput, build_instructions};

/// Build a `PipelineInput` rooted at `tmp`.
///
/// #4588: the pipeline resolves its roster from the PROJECT via the one shared
/// resolver, so the input names a project directory rather than an agents
/// directory. Agents for these tests go into that project's own
/// `.claude/agents` tier — see [`agents_tier`].
fn input_in(tmp: &TempDir) -> PipelineInput {
    PipelineInput {
        project_dir: tmp.path().join("project"),
        claude_md_path: tmp.path().join("project").join("CLAUDE.md"),
    }
}

/// The highest-precedence roster tier for `input`'s project.
fn agents_tier(input: &PipelineInput) -> PathBuf {
    input.project_dir.join(".claude").join("agents")
}

fn write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, content).expect("write file");
}

/// With deployed agents present, the pipeline counts them and seeds the
/// `CLAUDE.md` stub when it is absent.
#[test]
fn pipeline_counts_agents_and_seeds_the_stub() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);

    std::fs::create_dir_all(agents_tier(&input)).unwrap();
    write(
        &agents_tier(&input).join("engineer.md"),
        "---\nname: engineer\nrole: engineer\ndescription: Builds things.\n---\n\n# Engineer\n",
    );
    // No CLAUDE.md — the pipeline must still seed the stub.
    assert!(!input.claude_md_path.exists());

    let out = build_instructions(&input).expect("pipeline succeeds");
    assert!(out.claude_md_created, "stub created when CLAUDE.md absent");
    assert!(input.claude_md_path.exists(), "stub written to disk");
    assert!(out.agent_count >= 1, "the deployed engineer is counted");
}

/// An existing project `CLAUDE.md` with custom content is left completely
/// untouched on disk (issue #2170 — trusty-mpm must never modify a target
/// project's `CLAUDE.md`; the delegation directive is delivered exclusively
/// via the `trusty-mpm` output style).
#[test]
fn pipeline_preserves_claude_md() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);

    std::fs::create_dir_all(agents_tier(&input)).unwrap();
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
}

/// #4832: an unreadable retired framework `instructions/INSTRUCTIONS.md` must
/// not fail the pipeline — nothing has written that file since #4752, and its
/// content was never used.
///
/// FAILS BEFORE THIS CHANGE: the pipeline read that path, and a directory
/// planted there produced `IsADirectory`, which became the one fatal
/// `PrepError` variant and refused the session.
#[test]
fn pipeline_ignores_the_retired_framework_instructions() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);
    std::fs::create_dir_all(agents_tier(&input)).unwrap();

    let retired =
        trusty_mpm::core::paths::FrameworkPaths::under(tmp.path()).framework_instructions_path();
    std::fs::create_dir_all(&retired).expect("plant a directory at the retired path");

    build_instructions(&input).expect("an unreadable retired path must not fail the pipeline");
}

/// A deployed `engineer.md` agent is counted by the pipeline. This wires the
/// deploy step and the pipeline together end to end.
#[test]
fn pipeline_counts_deployed_agents() {
    let tmp = TempDir::new().unwrap();
    let input = input_in(&tmp);

    // Deploy a real composed engineer agent into the pipeline's agents dir.
    let src = TempDir::new().unwrap();
    write_agent_sources(src.path());
    deploy_agents(src.path(), &agents_tier(&input)).expect("deploy succeeds");

    let out = build_instructions(&input).expect("pipeline succeeds");
    assert!(out.agent_count >= 1, "deployed agents counted");

    // The delegation section the PM actually receives is rendered by the same
    // resolver the count comes from (#4588), so assert it lists the agent.
    let delivered =
        trusty_mpm::core::delegation_authority::deployed_roster_section(&input.project_dir)
            .expect("a roster is deployed, so a section must render");
    assert!(
        delivered.contains("engineer"),
        "deployed engineer listed in the delivered delegation authority: {delivered}"
    );
}
