//! Tests for the bundle module.
//!
//! Why: `bundle.rs` is split to stay under the 500-line cap while keeping all
//! test coverage for embedded artifact integrity, policy correctness, and
//! agent inheritance-chain round-trips in one focused location.
//! What: unit and integration tests for every bundled artifact constant and
//! the [`crate::core::bundle::ALL`] table.
//! Test: this file is the test coverage; run with `cargo test -p trusty-mpm bundle`.
use super::*;

#[test]
fn constants_are_non_empty() {
    // Every embedded artifact must carry real content — an empty
    // `include_str!` target would mean a missing or truncated asset file.
    assert!(!OPTIMIZER_TOML.trim().is_empty());
    assert!(!OVERSEER_TOML.trim().is_empty());
    assert!(!FRAMEWORK_INSTRUCTIONS.trim().is_empty());
    assert!(!CLAUDE_STUB.trim().is_empty());
    assert!(!BASE_AGENT.trim().is_empty());
    assert!(!BASE_ENGINEER.trim().is_empty());
    assert!(!BASE_RESEARCH.trim().is_empty());
    assert!(!BASE_QA.trim().is_empty());
    assert!(!BASE_OPS.trim().is_empty());
    assert!(!ENGINEER_AGENT.trim().is_empty());
    assert!(!QA_AGENT.trim().is_empty());
    assert!(!RESEARCH_AGENT.trim().is_empty());
    assert!(!OPS_AGENT.trim().is_empty());
    assert!(!SECURITY_AGENT.trim().is_empty());
    assert!(!DOCUMENTATION_AGENT.trim().is_empty());
    assert!(!DATA_ENGINEER_AGENT.trim().is_empty());
    assert!(!VERSION_CONTROL_AGENT.trim().is_empty());
    assert!(!TICKETING_AGENT.trim().is_empty());
    assert!(!CODE_ANALYZER_AGENT.trim().is_empty());
    assert!(!PYTHON_ENGINEER_AGENT.trim().is_empty());
    assert!(!TYPESCRIPT_ENGINEER_AGENT.trim().is_empty());
    assert!(!GOLANG_ENGINEER_AGENT.trim().is_empty());
    assert!(!RUST_ENGINEER_AGENT.trim().is_empty());
    assert!(!JAVA_ENGINEER_AGENT.trim().is_empty());
    assert!(!PHP_ENGINEER_AGENT.trim().is_empty());
    assert!(!RUBY_ENGINEER_AGENT.trim().is_empty());
    assert!(!REACT_ENGINEER_AGENT.trim().is_empty());
    assert!(!NEXTJS_ENGINEER_AGENT.trim().is_empty());
    assert!(!SVELTE_ENGINEER_AGENT.trim().is_empty());
    assert!(!WEB_QA_AGENT.trim().is_empty());
    assert!(!API_QA_AGENT.trim().is_empty());
    // Increment 3 agents
    assert!(!JAVASCRIPT_ENGINEER_AGENT.trim().is_empty());
    assert!(!PHOENIX_ENGINEER_AGENT.trim().is_empty());
    assert!(!DART_ENGINEER_AGENT.trim().is_empty());
    assert!(!TAURI_ENGINEER_AGENT.trim().is_empty());
    assert!(!WEB_UI_ENGINEER_AGENT.trim().is_empty());
    assert!(!REFACTORING_ENGINEER_AGENT.trim().is_empty());
    assert!(!PROMPT_ENGINEER_AGENT.trim().is_empty());
    assert!(!CODE_CRITIC_AGENT.trim().is_empty());
    assert!(!GCP_OPS_AGENT.trim().is_empty());
    assert!(!VERCEL_OPS_AGENT.trim().is_empty());
    assert!(!LOCAL_OPS_AGENT.trim().is_empty());
    assert!(!MEMORY_MANAGER_AGENT.trim().is_empty());
    assert!(!MPM_AGENT_MANAGER_AGENT.trim().is_empty());
    assert!(!MPM_SKILLS_MANAGER_AGENT.trim().is_empty());
    assert!(!EXAMPLE_SKILL.trim().is_empty());
    assert!(!OUTPUT_STYLE.trim().is_empty());
}

#[test]
fn output_style_has_matching_frontmatter_name() {
    // Claude Code matches the `outputStyle` settings key against the
    // `name:` in the style file's frontmatter; a mismatch silently falls
    // back to the operator's default style.
    assert!(OUTPUT_STYLE.contains("name: trusty-mpm"));
}

#[test]
fn framework_instructions_and_stub_differ() {
    // The framework artifact and the user stub are distinct files with
    // distinct content; conflating them would lose either upgrades or
    // user edits.
    assert_ne!(FRAMEWORK_INSTRUCTIONS, CLAUDE_STUB);
}

#[test]
fn claude_stub_is_seed_once() {
    // The user stub must never be overwritten on re-install.
    let stub = ALL
        .iter()
        .find(|a| a.rel_path == "instructions/CLAUDE.md")
        .expect("CLAUDE.md stub present in bundle");
    assert_eq!(stub.install, InstallPolicy::SeedOnce);
}

#[test]
fn framework_instructions_overwrites() {
    // The framework instructions must be refreshed on every install.
    let instr = ALL
        .iter()
        .find(|a| a.rel_path == "instructions/INSTRUCTIONS.md")
        .expect("INSTRUCTIONS.md present in bundle");
    assert_eq!(instr.install, InstallPolicy::Overwrite);
}

#[test]
fn optimizer_toml_is_parseable() {
    // The shipped policy must be valid TOML or the installer would deploy
    // a file the daemon then fails to load.
    let parsed: toml::Value = toml::from_str(OPTIMIZER_TOML).expect("valid TOML");
    assert!(parsed.get("default").is_some());
}

#[test]
fn bundle_table_is_complete() {
    // `ALL` must enumerate every artifact with unique, non-empty paths.
    // Count: 4 hooks/instructions + 5 base agents + 36 concrete agents + 1 skill = 46
    // Increment 1 (9): qa, research, ops, security, documentation, data-engineer,
    //   version-control, ticketing, code-analyzer
    // Increment 2 (12): python-engineer, typescript-engineer, golang-engineer,
    //   rust-engineer, java-engineer, php-engineer, ruby-engineer,
    //   react-engineer, nextjs-engineer, svelte-engineer, web-qa, api-qa
    // Plus engineer.md (core engineer agent)
    // Increment 3 (14): javascript-engineer, phoenix-engineer, dart-engineer,
    //   tauri-engineer, web-ui-engineer, refactoring-engineer, prompt-engineer,
    //   code-critic, gcp-ops, vercel-ops, local-ops,
    //   memory-manager, mpm-agent-manager, mpm-skills-manager
    assert_eq!(ALL.len(), 46);
    let mut paths: Vec<&str> = ALL.iter().map(|a| a.rel_path).collect();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), 46, "artifact paths must be unique");
    for artifact in ALL {
        assert!(!artifact.rel_path.is_empty());
        assert!(!artifact.contents.trim().is_empty());
    }
}

#[test]
fn overseer_toml_is_parseable() {
    // The shipped overseer policy must be valid TOML and ship disabled —
    // oversight is opt-in, so installing it must not silently enable it.
    let parsed: toml::Value = toml::from_str(OVERSEER_TOML).expect("valid TOML");
    assert_eq!(
        parsed
            .get("overseer")
            .and_then(|o| o.get("enabled"))
            .and_then(toml::Value::as_bool),
        Some(false)
    );
}

#[test]
fn overseer_toml_is_in_bundle() {
    // `ALL` must include the overseer policy so `trusty-mpm install`
    // deploys it.
    assert!(
        ALL.iter().any(|a| a.rel_path == "hooks/overseer.toml"),
        "overseer.toml must be a bundled artifact"
    );
}

#[test]
fn new_concrete_agents_are_in_bundle() {
    // Every newly-ported concrete agent must be present in ALL so
    // `trusty-mpm install` deploys them offline.
    let agent_paths: Vec<&str> = ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("agents/"))
        .map(|a| a.rel_path)
        .collect();

    for expected in &[
        // Increment 1 agents
        "agents/qa.md",
        "agents/research.md",
        "agents/ops.md",
        "agents/security.md",
        "agents/documentation.md",
        "agents/data-engineer.md",
        "agents/version-control.md",
        "agents/ticketing.md",
        "agents/code-analyzer.md",
        // Increment 2 language engineers
        "agents/python-engineer.md",
        "agents/typescript-engineer.md",
        "agents/golang-engineer.md",
        "agents/rust-engineer.md",
        "agents/java-engineer.md",
        "agents/php-engineer.md",
        "agents/ruby-engineer.md",
        "agents/react-engineer.md",
        "agents/nextjs-engineer.md",
        "agents/svelte-engineer.md",
        // Increment 2 QA variants
        "agents/web-qa.md",
        "agents/api-qa.md",
        // Increment 3 agents
        "agents/javascript-engineer.md",
        "agents/phoenix-engineer.md",
        "agents/dart-engineer.md",
        "agents/tauri-engineer.md",
        "agents/web-ui-engineer.md",
        "agents/refactoring-engineer.md",
        "agents/prompt-engineer.md",
        "agents/code-critic.md",
        "agents/gcp-ops.md",
        "agents/vercel-ops.md",
        "agents/local-ops.md",
        "agents/memory-manager.md",
        "agents/mpm-agent-manager.md",
        "agents/mpm-skills-manager.md",
    ] {
        assert!(
            agent_paths.contains(expected),
            "missing bundled agent: {expected}"
        );
    }
}

#[test]
fn new_concrete_agents_have_extends_in_frontmatter() {
    // Each new concrete agent must declare `extends:` so the inheritance
    // chain resolves correctly at deploy time.
    let agents = [
        // Increment 1 agents
        ("qa", QA_AGENT),
        ("research", RESEARCH_AGENT),
        ("ops", OPS_AGENT),
        ("security", SECURITY_AGENT),
        ("documentation", DOCUMENTATION_AGENT),
        ("data-engineer", DATA_ENGINEER_AGENT),
        ("version-control", VERSION_CONTROL_AGENT),
        ("ticketing", TICKETING_AGENT),
        ("code-analyzer", CODE_ANALYZER_AGENT),
        // Increment 2 language engineers
        ("python-engineer", PYTHON_ENGINEER_AGENT),
        ("typescript-engineer", TYPESCRIPT_ENGINEER_AGENT),
        ("golang-engineer", GOLANG_ENGINEER_AGENT),
        ("rust-engineer", RUST_ENGINEER_AGENT),
        ("java-engineer", JAVA_ENGINEER_AGENT),
        ("php-engineer", PHP_ENGINEER_AGENT),
        ("ruby-engineer", RUBY_ENGINEER_AGENT),
        ("react-engineer", REACT_ENGINEER_AGENT),
        ("nextjs-engineer", NEXTJS_ENGINEER_AGENT),
        ("svelte-engineer", SVELTE_ENGINEER_AGENT),
        // Increment 2 QA variants
        ("web-qa", WEB_QA_AGENT),
        ("api-qa", API_QA_AGENT),
        // Increment 3 agents
        ("javascript-engineer", JAVASCRIPT_ENGINEER_AGENT),
        ("phoenix-engineer", PHOENIX_ENGINEER_AGENT),
        ("dart-engineer", DART_ENGINEER_AGENT),
        ("tauri-engineer", TAURI_ENGINEER_AGENT),
        ("web-ui-engineer", WEB_UI_ENGINEER_AGENT),
        ("refactoring-engineer", REFACTORING_ENGINEER_AGENT),
        ("prompt-engineer", PROMPT_ENGINEER_AGENT),
        ("code-critic", CODE_CRITIC_AGENT),
        ("gcp-ops", GCP_OPS_AGENT),
        ("vercel-ops", VERCEL_OPS_AGENT),
        ("local-ops", LOCAL_OPS_AGENT),
        ("memory-manager", MEMORY_MANAGER_AGENT),
        ("mpm-agent-manager", MPM_AGENT_MANAGER_AGENT),
        ("mpm-skills-manager", MPM_SKILLS_MANAGER_AGENT),
    ];
    for (name, content) in agents {
        assert!(
            content.contains("extends:"),
            "agent {name} is missing `extends:` in frontmatter"
        );
        // Composed output must not contain `extends:` (builder strips it).
        // We verify the raw source has it; compose round-trip is covered
        // by agent_deployer integration tests.
    }
}

#[test]
fn new_concrete_agents_deploy_via_real_asset_files() {
    // Verify each new agent composes without error using the real bundled
    // asset files on disk (not temp fixtures) — catches typos in `extends:`
    // values and missing base templates.
    use crate::core::agent_builder::compose_agent;
    use std::path::Path;

    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("assets")
        .join("agents");

    let agents = [
        // Increment 1 agents
        "qa",
        "research",
        "ops",
        "security",
        "documentation",
        "data-engineer",
        "version-control",
        "ticketing",
        "code-analyzer",
        // Increment 2 language engineers
        "python-engineer",
        "typescript-engineer",
        "golang-engineer",
        "rust-engineer",
        "java-engineer",
        "php-engineer",
        "ruby-engineer",
        "react-engineer",
        "nextjs-engineer",
        "svelte-engineer",
        // Increment 2 QA variants
        "web-qa",
        "api-qa",
        // Increment 3 agents
        "javascript-engineer",
        "phoenix-engineer",
        "dart-engineer",
        "tauri-engineer",
        "web-ui-engineer",
        "refactoring-engineer",
        "prompt-engineer",
        "code-critic",
        "gcp-ops",
        "vercel-ops",
        "local-ops",
        "memory-manager",
        "mpm-agent-manager",
        "mpm-skills-manager",
    ];

    for name in agents {
        let composed = compose_agent(name, &assets_dir)
            .unwrap_or_else(|e| panic!("compose_agent({name}) failed: {e}"));
        // Composed output must have a frontmatter block.
        assert!(
            composed.starts_with("---\n"),
            "composed {name} is missing frontmatter"
        );
        // `extends:` must not leak into the composed output.
        assert!(
            !composed.contains("extends:"),
            "composed {name} has leaked `extends:` in output"
        );
        // Must have non-trivial body content.
        assert!(
            composed.len() > 200,
            "composed {name} suspiciously short ({} bytes)",
            composed.len()
        );
    }
}
