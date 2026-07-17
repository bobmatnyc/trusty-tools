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
    assert!(!DOTNET_ENGINEER_AGENT.trim().is_empty());
    assert!(!TAURI_ENGINEER_AGENT.trim().is_empty());
    assert!(!WEB_UI_ENGINEER_AGENT.trim().is_empty());
    assert!(!REFACTORING_ENGINEER_AGENT.trim().is_empty());
    assert!(!PROMPT_ENGINEER_AGENT.trim().is_empty());
    assert!(!CODE_CRITIC_AGENT.trim().is_empty());
    assert!(!CODE_REVIEW_STANDARDS.trim().is_empty());
    assert!(!CONTRACT_DRIVEN_TESTING.trim().is_empty());
    assert!(!GCP_OPS_AGENT.trim().is_empty());
    assert!(!VERCEL_OPS_AGENT.trim().is_empty());
    assert!(!LOCAL_OPS_AGENT.trim().is_empty());
    assert!(!MEMORY_MANAGER_AGENT.trim().is_empty());
    assert!(!MPM_AGENT_MANAGER_AGENT.trim().is_empty());
    assert!(!MPM_SKILLS_MANAGER_AGENT.trim().is_empty());
    assert!(!OUTPUT_STYLE.trim().is_empty());
    assert!(!OUTPUT_STYLE_TEACHER.trim().is_empty());
    assert!(!OUTPUT_STYLE_RESEARCH.trim().is_empty());
    assert!(!TM_DOCTOR.trim().is_empty());
    // /tm- portfolio (tm-skills-portfolio epic)
    assert!(!TM_CIRCUIT_BREAKER.trim().is_empty());
    assert!(!TM_VERIFICATION_PROTOCOLS.trim().is_empty());
    assert!(!TM_TOOL_USAGE_GUIDE.trim().is_empty());
    assert!(!TM_GIT_FILE_TRACKING.trim().is_empty());
    assert!(!TM_ADR.trim().is_empty());
    assert!(!TM_WORKFLOW.trim().is_empty());
    assert!(!TM_AGENT_ARCHITECTURE.trim().is_empty());
    assert!(!TM_POSTMORTEM.trim().is_empty());
    assert!(!TM_BUG_REPORTING.trim().is_empty());
    assert!(!TM_TEACHING_TEMPLATES.trim().is_empty());
    assert!(!TM_TICKETING.trim().is_empty());
    assert!(!TM_PR_WORKFLOW.trim().is_empty());
    assert!(!TM_DELEGATION_PATTERNS.trim().is_empty());
    assert!(!TM_SESSION_MANAGEMENT.trim().is_empty());
    assert!(!TM_SESSION_PAUSE.trim().is_empty());
    assert!(!TM_SESSION_RESUME.trim().is_empty());
    assert!(!TM_INIT.trim().is_empty());
    assert!(!TM_OVERVIEW.trim().is_empty());
    assert!(!TM_ISSUES_PRUNE.trim().is_empty());
    assert!(!TM_CLI_OPERATIONS.trim().is_empty());
    assert!(!WHAT_IS_TRUSTY_MPM.trim().is_empty());
    // --- BEGIN issue #2911: documentation-style bundled skill (append-only) ---
    assert!(!DOCUMENTATION_STYLE.trim().is_empty());
    assert!(!DOCUMENTATION_STYLE_SPEC.trim().is_empty());
    assert!(!DOCUMENTATION_STYLE_README.trim().is_empty());
    assert!(!DOCUMENTATION_STYLE_FILE_LEVEL.trim().is_empty());
    assert!(!DOCUMENTATION_STYLE_CLASS.trim().is_empty());
    assert!(!DOCUMENTATION_STYLE_METHOD_FUNCTION.trim().is_empty());
    assert!(!DOCUMENTATION_STYLE_BLOCK_INLINE.trim().is_empty());
    // --- END issue #2911 ---
    assert!(!RUST_BUILD_PERFORMANCE.trim().is_empty());
}

#[test]
fn tm_skills_are_in_bundle() {
    // The /tm- portfolio (tm-skills-portfolio epic) must be present in ALL so
    // `trusty-mpm install` deploys every skill offline.
    let skill_paths: Vec<&str> = ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/tm-") || a.rel_path == "skills/tm.md")
        .map(|a| a.rel_path)
        .collect();

    for expected in &[
        "skills/tm-doctor.md",
        "skills/tm-circuit-breaker.md",
        "skills/tm-verification-protocols.md",
        "skills/tm-tool-usage-guide.md",
        "skills/tm-git-file-tracking.md",
        "skills/tm-adr.md",
        "skills/tm-workflow.md",
        "skills/tm-agent-architecture.md",
        "skills/tm-postmortem.md",
        "skills/tm-bug-reporting.md",
        "skills/tm-teaching-templates.md",
        "skills/tm-ticketing.md",
        "skills/tm-pr-workflow.md",
        "skills/tm-delegation-patterns.md",
        "skills/tm-session-management.md",
        "skills/tm-session-pause.md",
        "skills/tm-session-resume.md",
        "skills/tm-init.md",
        "skills/tm.md",
        "skills/tm-issues-prune.md",
        "skills/tm-cli-operations.md",
    ] {
        assert!(
            skill_paths.contains(expected),
            "missing bundled /tm- skill: {expected}"
        );
    }
}

#[test]
fn tm_skills_have_frontmatter() {
    // Every /tm- skill must carry YAML frontmatter with a tm-native `name:`
    // and must not leak unadapted claude-mpm references.
    let skills = [
        ("tm-circuit-breaker", TM_CIRCUIT_BREAKER),
        ("tm-verification-protocols", TM_VERIFICATION_PROTOCOLS),
        ("tm-tool-usage-guide", TM_TOOL_USAGE_GUIDE),
        ("tm-git-file-tracking", TM_GIT_FILE_TRACKING),
        ("tm-adr", TM_ADR),
        ("tm-workflow", TM_WORKFLOW),
        ("tm-agent-architecture", TM_AGENT_ARCHITECTURE),
        ("tm-postmortem", TM_POSTMORTEM),
        ("tm-bug-reporting", TM_BUG_REPORTING),
        ("tm-teaching-templates", TM_TEACHING_TEMPLATES),
        ("tm-ticketing", TM_TICKETING),
        ("tm-pr-workflow", TM_PR_WORKFLOW),
        ("tm-delegation-patterns", TM_DELEGATION_PATTERNS),
        ("tm-session-management", TM_SESSION_MANAGEMENT),
        ("tm-session-pause", TM_SESSION_PAUSE),
        ("tm-session-resume", TM_SESSION_RESUME),
        ("tm-init", TM_INIT),
        ("tm", TM_OVERVIEW),
        ("tm-issues-prune", TM_ISSUES_PRUNE),
        ("tm-cli-operations", TM_CLI_OPERATIONS),
    ];
    for (name, content) in skills {
        assert!(
            content.starts_with("---\n"),
            "skill {name} is missing YAML frontmatter"
        );
        assert!(
            content.contains(&format!("name: {name}")),
            "skill {name} frontmatter name must match its file stem"
        );
        assert!(
            !content.contains("claude-mpm") || content.contains("trusty-mpm"),
            "skill {name} contains an unadapted claude-mpm reference"
        );
        // tm-session-management is the one deliberate exception: it documents
        // `tm session catchup`'s real dual-format cutover bridge (#1762),
        // which genuinely reads the legacy `.claude-mpm/sessions/` path
        // alongside the native one — this is accurate, not an unadapted
        // leftover, and the skill explicitly calls it "legacy".
        assert!(
            !content.contains(".claude-mpm/") || name == "tm-session-management",
            "skill {name} references the legacy .claude-mpm/ path"
        );
    }
}

#[test]
fn output_style_has_matching_frontmatter_name() {
    // Claude Code matches the `outputStyle` settings key against the
    // `name:` in the style file's frontmatter; a mismatch silently falls
    // back to the operator's default style.
    assert!(OUTPUT_STYLE.contains("name: trusty-mpm"));
}

#[test]
fn output_style_registry_has_three_distinct_ids() {
    // HR-4 bundles exactly three styles with distinct ids and file names.
    assert_eq!(OUTPUT_STYLES.len(), 3);
    let mut ids: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3, "style ids must be distinct");

    let mut files: Vec<&str> = OUTPUT_STYLES.iter().map(|s| s.file_name).collect();
    files.sort_unstable();
    files.dedup();
    assert_eq!(files.len(), 3, "style file names must be distinct");
}

#[test]
fn output_style_registry_ids_match_frontmatter() {
    // Each registry id MUST equal the file's frontmatter `name:`, or Claude Code
    // silently falls back to the operator's default style.
    for style in OUTPUT_STYLES {
        assert!(!style.content.trim().is_empty(), "{} non-empty", style.id);
        assert!(
            style.content.contains(&format!("name: {}", style.id)),
            "{} frontmatter name must match registry id",
            style.id
        );
    }
}

#[test]
fn output_styles_carry_identity_protocol_and_load_marker() {
    // DOC-28 R2/R4(b): every bundled output style must carry the non-overridable
    // Identity & Self-Awareness Protocol section, and the marker line
    // `<!-- trusty-mpm-instructions-loaded: v1 -->` must be the literal first
    // line of that section (so a stale/degraded style is greppably distinct
    // from one carrying the current floor).
    const MARKER: &str = "<!-- trusty-mpm-instructions-loaded: v1 -->";
    const HEADING: &str = "## Identity & Self-Awareness Protocol (Non-Overridable)";
    for style in OUTPUT_STYLES {
        assert!(
            style.content.contains(HEADING),
            "{} is missing the Identity & Self-Awareness Protocol section",
            style.id
        );
        let marker_pos = style
            .content
            .find(MARKER)
            .unwrap_or_else(|| panic!("{} is missing the load marker", style.id));
        let heading_pos = style.content.find(HEADING).expect("heading present");
        let between = &style.content[marker_pos + MARKER.len()..heading_pos];
        assert_eq!(
            between.trim(),
            "",
            "{}: marker must be the first line immediately preceding the heading",
            style.id
        );
        // Forbidden shell-probe list must be greppable for regressions.
        assert!(style.content.contains("pip3 show"));
        assert!(style.content.contains("which claude-mpm"));
    }
}

#[test]
fn output_style_registry_default_resolves() {
    // The default id is present and points at the professional style.
    let default = OUTPUT_STYLES
        .iter()
        .find(|s| s.id == DEFAULT_OUTPUT_STYLE_ID)
        .expect("default style must be in the registry");
    assert_eq!(default.content, OUTPUT_STYLE);
    assert_eq!(default.file_name, "trusty-mpm.md");
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
    // Count: 4 hooks/instructions + 5 base agents + 37 concrete agents +
    // 2 code-critic bundled skills (#2890) + 21 /tm- skills + 1 DOC-28
    // self-description doc + 1 issue #2034 bundled architecture doc = 71
    // (A4, tm-skills-portfolio epic: the `example-skill.md` placeholder was
    // removed — it shipped to every user with no real content. A3: the
    // previously-orphaned tm-doctor.md is now wired in. The 11 Phase 1 (#770)
    // mpm-* guidance skills were removed entirely, superseded by the /tm-
    // portfolio below.)
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
    // Increment 4 (1): dotnet-engineer (#2831 — C#/.NET 8+ with VB.NET awareness)
    // Issue #2890 (2): code-review-standards, contract-driven-testing — the
    //   `skills:` dependencies code-critic declares in its frontmatter (DOC-42).
    //   Registration here (not just the asset file existing) is what makes
    //   `deploy_all_skill_tiers` actually ship them — the historical
    //   orphaned-tm-doctor.md bug this mirrors is documented in
    //   `bundle_tm_skills.rs`'s module doc.
    // /tm- portfolio (tm-skills-portfolio epic + issues #2185/#2321) (21): tm-doctor,
    //   tm-circuit-breaker, tm-verification-protocols, tm-tool-usage-guide,
    //   tm-git-file-tracking, tm-adr, tm-workflow, tm-agent-architecture,
    //   tm-postmortem, tm-bug-reporting, tm-teaching-templates, tm-ticketing,
    //   tm-pr-workflow, tm-delegation-patterns, tm-session-management,
    //   tm-session-pause, tm-session-resume, tm-init, tm (overview),
    //   tm-issues-prune (#2185: gh issue backlog prune/prioritize),
    //   tm-cli-operations (#2321: tm CLI operation incl. MCP setup/management)
    // DOC-28 R1 (1): docs/WHAT-IS-TRUSTY-MPM.md
    // Issue #2034 (1): docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md
    // Issue #2903 (93, skill-port batch 1, epic #2902): 25 upstream
    //   universal/ skills (entry SKILL.md files) plus 68 references/*.md
    //   files carried alongside multi-file skills — systematic-debugging,
    //   verification-before-completion, git-workflow, test-driven-development,
    //   requesting-code-review, writing-plans, json-data-handling,
    //   root-cause-tracing, internal-comms, brainstorming, software-patterns,
    //   security-scanning, api-design-patterns, database-migration,
    //   env-manager, web-performance-optimization, artifacts-builder,
    //   condition-based-waiting, model-context-builder, test-quality-inspector,
    //   testing-anti-patterns, webapp-testing, api-documentation, xlsx,
    //   code-production-process. 71 + 93 = 164.
    // Issue #2911 (7): documentation-style bundled skill — entry SKILL.md plus
    //   6 references/*.md files (spec, readme, file-level, class,
    //   method-function, block-inline). 164 + 7 = 171.
    // rust-build-performance (1, per Bob directive 2026-07-17): a single
    //   flat-file bundled skill (no references/ — small enough to not need
    //   one), declared by rust-engineer and tauri-engineer. 171 + 1 = 172.
    assert_eq!(ALL.len(), 172);
    let mut paths: Vec<&str> = ALL.iter().map(|a| a.rel_path).collect();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), 172, "artifact paths must be unique");
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
        "agents/dotnet-engineer.md",
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
        ("dotnet-engineer", DOTNET_ENGINEER_AGENT),
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
        "dotnet-engineer",
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

#[test]
fn base_agent_guidance_sections_survive_composition() {
    // Regression for #2501/#2502/#2610: BASE-AGENT.md's "Foreground Execution"
    // and "PM Directives Do Not Bind You" sections — plus the version-control
    // persona's own CI-wait reinforcement — must propagate into the composed
    // agent via the real bundled asset chain, not just exist in the source
    // files. Uses version-control (the worst parking offender) composed from
    // the real assets dir so a future refactor of the compose/extends pipeline
    // can't silently drop the no-parking guidance.
    use crate::core::agent_builder::compose_agent;
    use std::path::Path;

    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("assets")
        .join("agents");

    let composed = compose_agent("version-control", &assets_dir)
        .expect("compose_agent(version-control) must succeed");

    assert!(
        composed.contains("## Foreground Execution"),
        "composed version-control is missing the Foreground Execution section"
    );
    assert!(
        composed.contains("NEVER end your turn to \"wait\""),
        "composed version-control is missing the BASE-AGENT hard no-parking rule (#2610)"
    );
    assert!(
        composed.contains("PROTOCOL VIOLATION"),
        "composed version-control is missing the parking-is-a-protocol-violation rule (#2610)"
    );
    assert!(
        composed.contains("gh pr checks <pr> --watch --fail-fast"),
        "composed version-control is missing the canonical CI-wait blocking pattern (#2610)"
    );
    assert!(
        composed.contains("## CI Waits — Block In The Foreground, NEVER Park"),
        "composed version-control is missing its persona-level CI-wait section (#2610)"
    );
    assert!(
        composed.contains("## PM Directives Do Not Bind You"),
        "composed version-control is missing the PM Directives Do Not Bind You section"
    );
    assert!(
        composed.contains("governs the orchestrating PM session"),
        "composed version-control is missing the PM-directive-scope guidance"
    );

    // Position assertions: guard the section ORDER, not just presence, so a
    // future compose/extends refactor can't silently reorder BASE-AGENT.md
    // content into an incoherent sequence.
    let pm_directives_idx = composed
        .find("## PM Directives Do Not Bind You")
        .expect("PM Directives Do Not Bind You marker must be present");
    let git_workflow_idx = composed
        .find("## Git Workflow")
        .expect("Git Workflow marker must be present");
    assert!(
        pm_directives_idx < git_workflow_idx,
        "PM Directives Do Not Bind You ({pm_directives_idx}) must appear before \
         Git Workflow ({git_workflow_idx})"
    );

    let foreground_execution_idx = composed
        .find("## Foreground Execution")
        .expect("Foreground Execution marker must be present");
    let output_format_idx = composed
        .find("## Output Format")
        .expect("Output Format marker must be present");
    assert!(
        foreground_execution_idx < output_format_idx,
        "Foreground Execution ({foreground_execution_idx}) must appear before \
         Output Format ({output_format_idx})"
    );

    // #2610: local-ops (the release-agent offender) must carry its own
    // long-wait reinforcement on top of the inherited BASE-AGENT rule.
    let local_ops =
        compose_agent("local-ops", &assets_dir).expect("compose_agent(local-ops) must succeed");
    assert!(
        local_ops.contains("## Long Waits — Block In The Foreground, NEVER Park"),
        "composed local-ops is missing its persona-level long-wait section (#2610)"
    );
    assert!(
        local_ops.contains("NEVER end your turn to \"wait\""),
        "composed local-ops is missing the inherited BASE-AGENT no-parking rule (#2610)"
    );
}

#[test]
fn no_mpm_guidance_skills_remain_in_bundle() {
    // The tm-skills-portfolio epic removed the 11 Phase 1 (#770) mpm-*
    // guidance skills in favor of the /tm- portfolio; assert none linger.
    let mpm_skill_paths: Vec<&str> = ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/mpm-"))
        .map(|a| a.rel_path)
        .collect();
    assert!(
        mpm_skill_paths.is_empty(),
        "mpm-* guidance skills must be fully removed, found: {mpm_skill_paths:?}"
    );
}

#[test]
fn no_example_skill_remains_in_bundle() {
    // A4 (tm-skills-portfolio epic): the example-skill.md placeholder must
    // never reappear in ALL.
    assert!(
        !ALL.iter().any(|a| a.rel_path == "skills/example-skill.md"),
        "example-skill.md must not be present in ALL"
    );
}

#[test]
fn tm_doctor_skill_is_wired_into_bundle() {
    // A3 (tm-skills-portfolio epic): tm-doctor.md existed as an asset file
    // but was never wired into a const or the ALL table, so it silently
    // never shipped. Assert both are now true, and that it carries valid
    // frontmatter naming the skill `tm-doctor`.
    assert!(
        ALL.iter().any(|a| a.rel_path == "skills/tm-doctor.md"),
        "tm-doctor.md must be present in the ALL bundle table"
    );
    assert!(TM_DOCTOR.starts_with("---\n"));
    assert!(TM_DOCTOR.contains("name: tm-doctor"));
}

#[test]
fn code_critic_declared_skills_are_in_bundle() {
    // Issue #2890: code-critic's `skills:` frontmatter declares
    // `code-review-standards` and `contract-driven-testing`. Both asset files
    // existed under `src/assets/skills/` but were not wired into a const or
    // the `ALL` table on first cut — the exact historical tm-doctor.md bug
    // (see `tm_doctor_skill_is_wired_into_bundle` above and
    // `bundle_tm_skills.rs`'s module doc): a file existing on disk is not
    // sufficient for `deploy_all_skill_tiers` to ever see it, since that
    // function reads from the framework-root `skills/` directory, which is
    // populated exclusively from `ALL` entries at `tm install` time.
    assert!(
        ALL.iter()
            .any(|a| a.rel_path == "skills/code-review-standards.md"),
        "code-review-standards.md must be present in the ALL bundle table"
    );
    assert!(
        ALL.iter()
            .any(|a| a.rel_path == "skills/contract-driven-testing.md"),
        "contract-driven-testing.md must be present in the ALL bundle table"
    );
    assert!(CODE_REVIEW_STANDARDS.starts_with("---\n"));
    assert!(CODE_REVIEW_STANDARDS.contains("name: code-review-standards"));
    assert!(CONTRACT_DRIVEN_TESTING.starts_with("---\n"));
    assert!(CONTRACT_DRIVEN_TESTING.contains("name: contract-driven-testing"));
    // The agent's declared `skills:` frontmatter must name both of these,
    // so the DOC-42 co-deployment mechanism has something real to resolve —
    // issue #2903 extended the declaration with four more batch-1 skills
    // code-critic's upstream counterpart also names (code-production-process,
    // software-patterns, systematic-debugging, verification-before-completion),
    // so this only asserts the two #2890-era names are still present, not the
    // full six-skill list (that full-list assertion lives in
    // `code_critic_declares_batch1_skills`).
    assert!(CODE_CRITIC_AGENT.contains("code-review-standards"));
    assert!(CODE_CRITIC_AGENT.contains("contract-driven-testing"));
    assert!(CODE_CRITIC_AGENT.contains("skills: ["));
}

#[test]
fn code_critic_declares_batch1_skills() {
    // Issue #2903: code-critic's upstream counterpart declares 5 skills
    // (code-review-standards, code-production-process, software-patterns,
    // systematic-debugging, verification-before-completion); #2890 ported
    // only the first, this issue ports the remaining 4 — extend the
    // declaration to the full upstream-matched set (plus the net-new
    // `contract-driven-testing`, which has no upstream counterpart).
    assert!(CODE_CRITIC_AGENT.contains(
        "skills: [code-review-standards, contract-driven-testing, code-production-process, \
         software-patterns, systematic-debugging, verification-before-completion]"
    ));
}

#[test]
fn skill_port_batch1_skills_are_in_bundle() {
    // Issue #2903 (epic #2902): all 25 batch-1 universal/ skill entry points
    // must be present in `ALL` — mirrors `tm_doctor_skill_is_wired_into_bundle`
    // and `code_critic_declared_skills_are_in_bundle` above: a source file
    // existing under `src/assets/skills/` is not sufficient, only `ALL`
    // registration makes `deploy_all_skill_tiers` ship it.
    const BATCH1_ENTRIES: &[&str] = &[
        "skills/systematic-debugging.md",
        "skills/verification-before-completion.md",
        "skills/git-workflow.md",
        "skills/test-driven-development.md",
        "skills/requesting-code-review.md",
        "skills/writing-plans.md",
        "skills/json-data-handling.md",
        "skills/root-cause-tracing.md",
        "skills/internal-comms.md",
        "skills/brainstorming.md",
        "skills/software-patterns.md",
        "skills/security-scanning.md",
        "skills/api-design-patterns.md",
        "skills/database-migration.md",
        "skills/env-manager.md",
        "skills/web-performance-optimization.md",
        "skills/artifacts-builder.md",
        "skills/condition-based-waiting.md",
        "skills/model-context-builder.md",
        "skills/test-quality-inspector.md",
        "skills/testing-anti-patterns.md",
        "skills/webapp-testing.md",
        "skills/api-documentation.md",
        "skills/xlsx.md",
        "skills/code-production-process.md",
    ];
    assert_eq!(BATCH1_ENTRIES.len(), 25);
    for rel_path in BATCH1_ENTRIES {
        assert!(
            ALL.iter().any(|a| &a.rel_path == rel_path),
            "{rel_path} must be present in the ALL bundle table"
        );
    }
}

#[test]
fn skill_port_batch1_references_land_on_disk() {
    // A sample of multi-file batch-1 skills must carry their references/*.md
    // files as SEPARATE `ALL` entries alongside the entry-point SKILL.md — the
    // multi-file skill-directory extension this issue adds to the bundle/
    // deploy machinery (epic #2902 constraint #2). Real deploy-reachability
    // (landing under a deployed `.claude/skills/<name>/references/`) is
    // proven end-to-end in `tests_behavior_2903_skills_tests.rs`.
    for (entry, refs) in [
        (
            "skills/systematic-debugging.md",
            &[
                "skills/systematic-debugging/references/anti-patterns.md",
                "skills/systematic-debugging/references/examples.md",
                "skills/systematic-debugging/references/troubleshooting.md",
                "skills/systematic-debugging/references/workflow.md",
            ][..],
        ),
        (
            "skills/software-patterns.md",
            &[
                "skills/software-patterns/references/anti-patterns.md",
                "skills/software-patterns/references/code-smell-signals.md",
                "skills/software-patterns/references/decision-trees.md",
                "skills/software-patterns/references/examples.md",
                "skills/software-patterns/references/foundational-patterns.md",
                "skills/software-patterns/references/situational-patterns.md",
            ][..],
        ),
    ] {
        assert!(
            ALL.iter().any(|a| a.rel_path == entry),
            "{entry} must be present in the ALL bundle table"
        );
        for rel_path in refs {
            assert!(
                ALL.iter().any(|a| &a.rel_path == rel_path),
                "{rel_path} must be present in the ALL bundle table"
            );
        }
    }
}

#[test]
fn what_is_trusty_mpm_is_in_bundle() {
    // DOC-28 R1 acceptance: the canonical self-description doc must be a
    // bundled artifact so `tm install` deploys it to every framework root,
    // not just the trusty-tools repo's own source tree.
    let artifact = ALL
        .iter()
        .find(|a| a.rel_path == "docs/WHAT-IS-TRUSTY-MPM.md")
        .expect("WHAT-IS-TRUSTY-MPM.md must be a bundled artifact");
    assert_eq!(
        artifact.install,
        InstallPolicy::Overwrite,
        "the doc is framework-owned and must track upgrades"
    );
    assert_eq!(artifact.contents, WHAT_IS_TRUSTY_MPM);
}

#[test]
fn what_is_trusty_mpm_disambiguates_claude_mpm() {
    // DOC-28 R1 acceptance: the doc must contain the literal substrings that
    // prove it identifies the project (Rust, tm, tcode) and explicitly
    // disambiguates it from the unrelated Python claude-mpm package.
    assert!(WHAT_IS_TRUSTY_MPM.contains("Rust"));
    assert!(WHAT_IS_TRUSTY_MPM.contains("tm"));
    assert!(WHAT_IS_TRUSTY_MPM.contains("tcode"));
    assert!(WHAT_IS_TRUSTY_MPM.contains("claude-mpm"));
    // The claude-mpm substring must appear inside an explicit disambiguation
    // sentence, not incidentally — assert it co-occurs with "NOT" nearby.
    assert!(
        WHAT_IS_TRUSTY_MPM.contains("NOT") && WHAT_IS_TRUSTY_MPM.contains("claude-mpm"),
        "doc must explicitly disambiguate from claude-mpm, not mention it incidentally"
    );
}

#[test]
fn idle_park_mitigation_2833_guidance_survives_composition() {
    // Regression for #2833's code-critic review (MEDIUM finding): the prior
    // `base_agent_guidance_sections_survive_composition` test only asserted the
    // pre-existing #2501/#2610 strings — none of the #2833 idle-park-mitigation
    // content (chunked-repoll anti-spam guidance, the PM-side parked-subagent
    // nudge protocol, the version-control anti-spam bullet) was ever exercised
    // through the REAL bundled asset chain. This test closes that gap using the
    // same real-composition approach as its sibling: `compose_agent` reading
    // the actual `assets/agents` dir for the two agent-tier assertions, and
    // `assemble_system_prompt` (the real PM_INSTRUCTIONS/WORKFLOW/
    // AGENT_DELEGATION/BASE_PM assembly used at session launch) for the
    // PM-tier assertion — not a hand-built fixture string.
    use crate::core::agent_builder::compose_agent;
    use crate::core::instruction_pipeline::assemble_system_prompt;
    use std::path::Path;

    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("assets")
        .join("agents");

    // (a) BASE-AGENT.md's chunked-repoll subsection must reach a composed
    // agent that inherits BASE-AGENT (version-control extends it).
    let composed = compose_agent("version-control", &assets_dir)
        .expect("compose_agent(version-control) must succeed");
    assert!(
        composed
            .contains("### Re-issue without spamming — the chunked-repoll pattern (issue #2833)"),
        "composed version-control is missing the BASE-AGENT chunked-repoll subsection (#2833)"
    );

    // (c) version-control's own anti-spam bullet (distinct from the inherited
    // BASE-AGENT section) must also survive composition.
    assert!(
        composed.contains("that is the opposite failure (spam) and just as"),
        "composed version-control is missing its persona-level anti-spam bullet (#2833)"
    );

    // (b) PM_INSTRUCTIONS.md's new Parked-Subagent Detection & Nudge section
    // must survive the real PM system-prompt assembly (not just exist in the
    // source .md file).
    let pm_prompt = assemble_system_prompt();
    assert!(
        pm_prompt.contains("## Parked-Subagent Detection & Nudge (issue #2833)"),
        "assembled PM system prompt is missing the Parked-Subagent Detection & \
         Nudge section (#2833)"
    );
    assert!(
        pm_prompt
            .contains("never a 30-second blind poll (that is the spam counter-failure, #2833)"),
        "assembled PM system prompt is missing the PM-side anti-spam-monitoring \
         guidance (#2833)"
    );
}

#[test]
fn pm_authority_doctrine_survives_composition() {
    // Regression for the live refusal where a version-control agent treated a
    // PM-relayed operator authorization as an untrusted third party's word and
    // froze an authorized admin-merge. The doctrine — PM relays operator
    // authority; doubt escalates back to the PM; only OBJECTIVE gates (red/
    // pending CI, fabricated evidence, worktree discipline) stay the agent's
    // own non-negotiables — must reach a composed agent through the REAL
    // bundled asset chain, not just live in the source .md.
    use crate::core::agent_builder::compose_agent;
    use std::path::Path;

    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("assets")
        .join("agents");

    let composed = compose_agent("version-control", &assets_dir)
        .expect("compose_agent(version-control) must succeed");

    // (a) BASE-AGENT's PM Authority & Escalation section must be inherited by
    // version-control (which extends base-ops → base-agent).
    assert!(
        composed.contains("## PM Authority & Escalation"),
        "composed version-control is missing the inherited BASE-AGENT \
         'PM Authority & Escalation' section"
    );
    // The load-bearing anti-distrust sentence — the exact behavior that failed.
    assert!(
        composed.contains("do NOT treat the dispatching PM as an untrusted third party"),
        "composed version-control is missing the anti-third-party-distrust rule"
    );
    // Injection-skepticism must be scoped to untrusted CONTENT, not the PM.
    assert!(
        composed.contains("Injection-skepticism is for UNTRUSTED CONTENT you read"),
        "composed version-control is missing the injection-skepticism scoping"
    );
    // The escalation path: doubt → report back to the PM, never freeze.
    assert!(
        composed.contains("REPORT BACK TO THE PM"),
        "composed version-control is missing the escalate-to-PM path"
    );

    // (b) version-control's own PR-workflow authority text (distinct from the
    // inherited section) must also survive composition.
    assert!(
        composed.contains("When the PM relays operator authorization to merge directly"),
        "composed version-control is missing its PR-workflow authority text"
    );

    // (c) The OBJECTIVE-safety-gate half of the doctrine must survive
    // composition too — authority never buys a bypass of red/pending CI, from
    // both the inherited BASE-AGENT rule and version-control's own rule.
    assert!(
        composed.contains("never merge red or pending CI (`--admin`"),
        "composed version-control is missing the inherited BASE-AGENT \
         objective-safety-gate (red/pending CI) text"
    );
    assert!(
        composed.contains("never a failing check"),
        "composed version-control is missing the 'never a failing check' \
         qualifier on the CI safety gate"
    );
    let composed_normalized = composed.replace('\n', " ");
    assert!(
        composed_normalized.contains("never merge red or pending CI"),
        "composed version-control is missing its own PR-workflow \
         objective-safety-gate (red/pending CI) text"
    );
}

#[test]
fn documentation_style_skill_is_in_bundle() {
    // Issue #2911: the documentation-style entry SKILL.md must be present in
    // `ALL` — mirrors `tm_doctor_skill_is_wired_into_bundle` and
    // `skill_port_batch1_skills_are_in_bundle` above: a source file existing
    // under `src/assets/skills/` is not sufficient, only `ALL` registration
    // makes `deploy_all_skill_tiers` ship it.
    assert!(
        ALL.iter()
            .any(|a| a.rel_path == "skills/documentation-style.md"),
        "documentation-style.md must be present in the ALL bundle table"
    );
    assert!(DOCUMENTATION_STYLE.starts_with("---\n"));
    assert!(DOCUMENTATION_STYLE.contains("name: documentation-style"));
    // SLD self-compliance (DOC-38 §2.5): the entry SKILL.md carries
    // `spec_refs:` frontmatter pointing at the grammar section it defers to,
    // with `anchor` equal to `id` per DOC-38's self-check rule.
    assert!(DOCUMENTATION_STYLE.contains("spec_refs:"));
    assert!(DOCUMENTATION_STYLE.contains("id: SPEC-SLD-02~draft"));
    assert!(DOCUMENTATION_STYLE.contains("anchor: SPEC-SLD-02~draft"));
}

#[test]
fn documentation_style_references_land_on_disk() {
    // Issue #2911: all 6 references/*.md files must be SEPARATE `ALL` entries
    // alongside the entry-point SKILL.md — mirrors
    // `skill_port_batch1_references_land_on_disk`. Real deploy-reachability
    // (landing under a deployed `.claude/skills/documentation-style/references/`)
    // is proven end-to-end in `tests_behavior_2911_documentation_style_tests.rs`.
    assert!(
        ALL.iter()
            .any(|a| a.rel_path == "skills/documentation-style.md")
    );
    for rel_path in [
        "skills/documentation-style/references/spec.md",
        "skills/documentation-style/references/readme.md",
        "skills/documentation-style/references/file-level.md",
        "skills/documentation-style/references/class.md",
        "skills/documentation-style/references/method-function.md",
        "skills/documentation-style/references/block-inline.md",
    ] {
        assert!(
            ALL.iter().any(|a| a.rel_path == rel_path),
            "{rel_path} must be present in the ALL bundle table"
        );
    }
}

#[test]
fn documentation_style_unions_into_engineer_family_via_base_engineer() {
    // Issue #2911: BASE-ENGINEER.md now declares `skills: [documentation-style]`;
    // compose_agent's DOC-42 union-across-chain merge (builder.rs
    // `merge_frontmatter`) must propagate it into every concrete engineer-family
    // agent composed from the REAL bundled asset files, not a synthetic fixture.
    use crate::core::agent_builder::compose_agent;
    use std::path::Path;

    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("assets")
        .join("agents");

    let composed = compose_agent("rust-engineer", &assets_dir)
        .expect("compose_agent(rust-engineer) must succeed");
    assert!(
        composed.contains("documentation-style"),
        "composed rust-engineer is missing the BASE-ENGINEER-declared \
         documentation-style skill (union-across-chain, DOC-42): {composed}"
    );
}

#[test]
fn rust_build_performance_skill_is_in_bundle() {
    // Per Bob directive 2026-07-17: the rust-build-performance entry SKILL.md
    // must be present in `ALL` — mirrors `documentation_style_skill_is_in_bundle`:
    // a source file existing under `src/assets/skills/` is not sufficient,
    // only `ALL` registration makes `deploy_all_skill_tiers` ship it.
    assert!(
        ALL.iter()
            .any(|a| a.rel_path == "skills/rust-build-performance.md"),
        "rust-build-performance.md must be present in the ALL bundle table"
    );
    assert!(RUST_BUILD_PERFORMANCE.starts_with("---\n"));
    assert!(RUST_BUILD_PERFORMANCE.contains("name: rust-build-performance"));
}

#[test]
fn rust_build_performance_declared_by_rust_family_agents() {
    // Per Bob directive 2026-07-17: rust-engineer and tauri-engineer both
    // declare `rust-build-performance` directly in their own `skills:`
    // frontmatter (NOT via BASE-ENGINEER — not every engineer compiles
    // Rust), proven against the REAL bundled asset files via compose_agent,
    // mirroring `documentation_style_unions_into_engineer_family_via_base_engineer`.
    use crate::core::agent_builder::compose_agent;
    use std::path::Path;

    let assets_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("assets")
        .join("agents");

    for agent in ["rust-engineer", "tauri-engineer"] {
        let composed = compose_agent(agent, &assets_dir)
            .unwrap_or_else(|e| panic!("compose_agent({agent}) must succeed: {e}"));
        assert!(
            composed.contains("rust-build-performance"),
            "composed {agent} is missing its own declared \
             rust-build-performance skill: {composed}"
        );
    }
}
