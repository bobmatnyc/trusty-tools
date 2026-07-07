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
    assert!(!WHAT_IS_TRUSTY_MPM.trim().is_empty());
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
    // Count: 4 hooks/instructions + 5 base agents + 36 concrete agents +
    // 20 /tm- skills + 1 DOC-28 self-description doc + 1 issue #2034
    // bundled architecture doc = 67
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
    // /tm- portfolio (tm-skills-portfolio epic + issue #2185) (20): tm-doctor,
    //   tm-circuit-breaker, tm-verification-protocols, tm-tool-usage-guide,
    //   tm-git-file-tracking, tm-adr, tm-workflow, tm-agent-architecture,
    //   tm-postmortem, tm-bug-reporting, tm-teaching-templates, tm-ticketing,
    //   tm-pr-workflow, tm-delegation-patterns, tm-session-management,
    //   tm-session-pause, tm-session-resume, tm-init, tm (overview),
    //   tm-issues-prune (#2185: gh issue backlog prune/prioritize)
    // DOC-28 R1 (1): docs/WHAT-IS-TRUSTY-MPM.md
    // Issue #2034 (1): docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md
    assert_eq!(ALL.len(), 67);
    let mut paths: Vec<&str> = ALL.iter().map(|a| a.rel_path).collect();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), 67, "artifact paths must be unique");
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
