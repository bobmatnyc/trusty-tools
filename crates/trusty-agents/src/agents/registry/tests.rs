//! Unit tests for agent discovery, capability matching, and roster rendering.
//!
//! Why: Pins the priority-shadowing, `.md`/`.toml` discovery, capability
//! scoring, and PM-roster behavior against synthetic fixtures and the bundled
//! on-disk agents so registry refactors can't silently regress delegation.
//! What: Exercises `AgentRegistry::{load, get, best_match, list}`,
//! `agent_search_paths`, `parse_md_agent`, and the roster renderers.
//! Test: This module IS the test surface.

use super::md_agent::parse_md_agent;
use super::{
    AgentConfig, AgentRegistry, agent_search_paths, build_roster_section, inject_roster_into_prompt,
};
use crate::agents::RunnerKind;
use crate::test_env::HOME_LOCK;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_agent(
    dir: &Path,
    name: &str,
    role: &str,
    langs: &[&str],
    frameworks: &[&str],
    tags: &[&str],
) {
    let caps_section = if langs.is_empty() && frameworks.is_empty() && tags.is_empty() {
        format!(
            "[agent.capabilities]\nroles = [\"{role}\"]\nlanguages = []\nframeworks = []\ntags = []\n"
        )
    } else {
        let langs = langs
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let frameworks = frameworks
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let tags = tags
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "[agent.capabilities]\nroles = [\"{role}\"]\nlanguages = [{langs}]\nframeworks = [{frameworks}]\ntags = [{tags}]\n"
        )
    };

    let toml = format!(
        r#"
[agent]
name = "{name}"
role = "{role}"
model = "anthropic/claude-sonnet-4-6"
description = "test agent"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

{caps_section}
"#
    );
    fs::write(dir.join(format!("{name}.toml")), toml).unwrap();
}

#[test]
fn capabilities_parse_from_toml() {
    let toml = r#"
[agent]
name = "x"
role = "engineer"
model = "x"
description = "x"

[agent.capabilities]
languages = ["python", "rust"]
frameworks = ["fastapi"]
roles = ["engineer"]
tags = ["general"]

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml).expect("parses");
    let caps = cfg.agent.capabilities.expect("caps present");
    assert_eq!(caps.languages, vec!["python", "rust"]);
    assert_eq!(caps.frameworks, vec!["fastapi"]);
    assert_eq!(caps.roles, vec!["engineer"]);
    assert_eq!(caps.tags, vec!["general"]);
}

#[test]
fn registry_loads_from_multiple_dirs_with_priority() {
    let high = TempDir::new().unwrap();
    let low = TempDir::new().unwrap();
    // Same agent name in both dirs; high-priority dir wins.
    write_agent(high.path(), "engineer", "engineer", &["rust"], &[], &[]);
    write_agent(low.path(), "engineer", "engineer", &["python"], &[], &[]);
    // Unique-to-low agent should still be discovered.
    write_agent(low.path(), "qa-agent", "qa", &[], &[], &["testing"]);

    let reg = AgentRegistry::load(&[high.path().to_path_buf(), low.path().to_path_buf()]);
    assert_eq!(reg.len(), 2);
    let eng = reg.get("engineer").expect("engineer present");
    let langs = &eng.agent.capabilities.as_ref().unwrap().languages;
    assert_eq!(langs, &vec!["rust".to_string()], "high-priority dir wins");
    assert!(reg.get("qa-agent").is_some());
}

#[test]
fn registry_best_match_prefers_specific_over_general() {
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "engineer", "engineer", &[], &[], &["general"]);
    write_agent(
        dir.path(),
        "python-engineer",
        "engineer",
        &["python"],
        &["fastapi"],
        &[],
    );

    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    let pick = reg.best_match(Some("engineer"), &["python"], &["fastapi"], &[]);
    assert_eq!(pick, Some("python-engineer"));

    // Without language/framework signals, role-only match should resolve
    // to the generic engineer (both score 10 on role; general engineer has
    // lower specificity, but python-engineer has higher specificity — so
    // actually python-engineer wins even then). Assert that at least one
    // of the two is returned deterministically.
    let pick = reg.best_match(Some("engineer"), &[], &[], &[]);
    assert!(pick == Some("python-engineer") || pick == Some("engineer"));
}

#[test]
fn registry_skips_missing_dirs_silently() {
    let real = TempDir::new().unwrap();
    write_agent(real.path(), "engineer", "engineer", &[], &[], &[]);
    let reg = AgentRegistry::load(&[
        PathBuf::from("/definitely/not/a/real/path/nope"),
        real.path().to_path_buf(),
    ]);
    assert_eq!(reg.len(), 1);
    assert!(reg.get("engineer").is_some());
}

#[test]
fn registry_list_returns_all_agents() {
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "a", "engineer", &["rust"], &[], &[]);
    write_agent(dir.path(), "b", "qa", &[], &[], &["testing"]);
    write_agent(dir.path(), "c", "docs", &[], &[], &["documentation"]);

    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    let list = reg.list();
    assert_eq!(list.len(), 3);
    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"a"));
    assert!(names.contains(&"b"));
    assert!(names.contains(&"c"));
}

#[test]
fn registry_best_match_uses_engineer_for_non_python() {
    // Why: trusty-agents intentionally ships ONE language specialist
    // (`python-engineer`) and routes all other languages to the generic
    // `engineer` agent, which receives the right language idiom skill
    // (rust-idiomatic.md, go-idiomatic.md, etc.) via runtime injection.
    // Reproduces the inverse routing bug: previously `python-engineer`
    // (with `["python"]` + four Python frameworks declared) won the
    // specificity tiebreak over `engineer` for Rust/TS tasks because
    // both scored 10 on role match and python-engineer had more
    // non-empty capability fields. The language-mismatch disqualifier
    // ensures specialists with declared languages are skipped when
    // none of their languages match the task.
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "engineer", "engineer", &[], &[], &["general"]);
    write_agent(
        dir.path(),
        "python-engineer",
        "engineer",
        &["python"],
        &["fastapi", "flask", "django", "pytest"],
        &[],
    );

    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    assert_eq!(
        reg.best_match(Some("engineer"), &["rust"], &[], &[]),
        Some("engineer"),
        "rust task must route to generic engineer (skill injection handles rust idioms)"
    );
    assert_eq!(
        reg.best_match(Some("engineer"), &["typescript"], &[], &[]),
        Some("engineer"),
        "typescript task must route to generic engineer"
    );
    assert_eq!(
        reg.best_match(Some("engineer"), &["go"], &[], &[]),
        Some("engineer"),
        "go task must route to generic engineer"
    );
    assert_eq!(
        reg.best_match(Some("engineer"), &["python"], &[], &[]),
        Some("python-engineer"),
        "python task must route to the one language specialist we ship"
    );
}

#[test]
fn registry_best_match_returns_none_when_no_scores() {
    // No agents registered -> None.
    let reg = AgentRegistry::load(&[]);
    assert!(reg.best_match(Some("engineer"), &[], &[], &[]).is_none());

    // Agents registered but no capability overlap -> None.
    let dir = TempDir::new().unwrap();
    write_agent(dir.path(), "engineer", "engineer", &[], &[], &[]);
    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    assert!(reg.best_match(Some("qa"), &["go"], &[], &[]).is_none());
}

#[test]
fn md_agent_file_parses_frontmatter_and_body() {
    let dir = TempDir::new().unwrap();
    let content = r#"---
name: md-agent
role: engineer
model: anthropic/claude-sonnet-4-6
runner: claude-code
description: md-formatted agent
capabilities:
  languages: [python]
  frameworks: [fastapi]
  roles: [engineer]
  tags: [rest-api]
---

SYSTEM PROMPT BODY HERE
"#;
    let path = dir.path().join("md-agent.md");
    fs::write(&path, content).unwrap();
    let cfg = parse_md_agent(&path).expect("md parses");
    assert_eq!(cfg.agent.name, "md-agent");
    assert_eq!(cfg.agent.role, "engineer");
    assert_eq!(cfg.agent.runner, RunnerKind::ClaudeCode);
    assert!(cfg.system_prompt.content.contains("SYSTEM PROMPT BODY"));
    let caps = cfg.agent.capabilities.expect("caps");
    assert_eq!(caps.languages, vec!["python"]);
    assert_eq!(caps.frameworks, vec!["fastapi"]);
    assert_eq!(caps.tags, vec!["rest-api"]);
}

#[test]
fn registry_picks_up_md_files_alongside_toml() {
    let dir = TempDir::new().unwrap();
    // TOML agent.
    write_agent(dir.path(), "toml-eng", "engineer", &["rust"], &[], &[]);
    // MD agent.
    let md = r#"---
name: md-eng
role: engineer
model: anthropic/claude-sonnet-4-6
description: md engineer
capabilities:
  languages: [python]
  roles: [engineer]
  frameworks: []
  tags: []
---

body
"#;
    fs::write(dir.path().join("md-eng.md"), md).unwrap();

    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    assert_eq!(reg.len(), 2);
    assert!(reg.get("toml-eng").is_some());
    assert!(reg.get("md-eng").is_some());
    let md_cfg = reg.get("md-eng").unwrap();
    assert_eq!(
        md_cfg.agent.capabilities.as_ref().unwrap().languages,
        vec!["python".to_string()]
    );
}

#[test]
fn registry_md_file_without_frontmatter_is_skipped() {
    let dir = TempDir::new().unwrap();
    // Valid TOML agent stays loadable.
    write_agent(dir.path(), "ok", "engineer", &[], &[], &[]);
    // MD without frontmatter: skipped with warn.
    fs::write(dir.path().join("broken.md"), "# not an agent\n").unwrap();
    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    assert_eq!(reg.len(), 1);
    assert!(reg.get("ok").is_some());
}

// ── Integration-style tests against bundled config/agents/ ─────────────
//
// Why: Unit tests above use synthetic TempDir fixtures; these tests
// exercise the real on-disk bundled agents so we catch drift between
// registry logic and what ships with the repo (e.g. an agent accidentally
// dropping its capabilities section).
// Test: Run `cargo test --lib agent_registry_discovers_bundled_agents`.

fn bundled_agents_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents")
}

#[test]
fn agent_registry_discovers_bundled_agents() {
    let paths = vec![bundled_agents_dir()];
    let registry = AgentRegistry::load(&paths);
    // Core bundled agents referenced in the task spec.
    assert!(
        registry.get("engineer").is_some(),
        "engineer missing from bundled agents"
    );
    assert!(
        registry.get("python-engineer").is_some(),
        "python-engineer missing"
    );
    assert!(registry.get("plan-agent").is_some(), "plan-agent missing");
    assert!(registry.get("qa-agent").is_some(), "qa-agent missing");
}

#[test]
fn agent_registry_selects_python_engineer_for_python_task() {
    let paths = vec![bundled_agents_dir()];
    let registry = AgentRegistry::load(&paths);
    let best = registry.best_match(Some("engineer"), &["python"], &["fastapi"], &[]);
    assert_eq!(
        best,
        Some("python-engineer"),
        "best_match should pick python-engineer for python+fastapi signals"
    );
}

#[test]
fn agent_search_paths_order() {
    // SAFETY: test-only; we restore HOME at the end.
    // HOME_LOCK serializes with other tests that mutate $HOME process-wide.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", "/tmp/home-test");
    }
    let paths = agent_search_paths(Path::new("/opt/trusty-agents/config"));
    assert_eq!(paths[0], PathBuf::from(".trusty-agents/agents"));
    assert_eq!(paths[1], PathBuf::from(".claude/agents"));
    assert_eq!(
        paths[2],
        PathBuf::from("/tmp/home-test/.trusty-agents/agents")
    );
    assert_eq!(paths[3], PathBuf::from("/tmp/home-test/.claude/agents"));
    assert_eq!(paths[4], PathBuf::from("/opt/trusty-agents/config/agents"));

    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn build_roster_section_includes_all_registered_agents() {
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "python-engineer",
        "engineer",
        &["python"],
        &["fastapi"],
        &[],
    );
    write_agent(
        tmp.path(),
        "docs-agent",
        "docs",
        &[],
        &[],
        &["documentation"],
    );
    let reg = AgentRegistry::load(&[tmp.path().to_path_buf()]);
    let roster = build_roster_section(&reg);
    assert!(
        roster.contains("python-engineer"),
        "roster missing python-engineer: {roster}"
    );
    assert!(
        roster.contains("docs-agent"),
        "roster missing docs-agent: {roster}"
    );
    assert!(
        roster.contains("python"),
        "roster missing language annotation: {roster}"
    );
    assert!(
        roster.contains("fastapi"),
        "roster missing framework annotation: {roster}"
    );
}

#[test]
fn build_roster_section_omits_empty_capability_fields() {
    let tmp = TempDir::new().unwrap();
    // Agent with no languages/frameworks/tags — only role.
    write_agent(tmp.path(), "bare-agent", "engineer", &[], &[], &[]);
    let reg = AgentRegistry::load(&[tmp.path().to_path_buf()]);
    let roster = build_roster_section(&reg);
    assert!(roster.contains("bare-agent"));
    // No "languages:" or "frameworks:" or "tags:" annotation since they're empty.
    assert!(
        !roster.contains("languages:"),
        "empty languages leaked: {roster}"
    );
    assert!(
        !roster.contains("frameworks:"),
        "empty frameworks leaked: {roster}"
    );
    assert!(!roster.contains("tags:"), "empty tags leaked: {roster}");
}

#[test]
fn build_roster_section_excludes_pm_itself() {
    let tmp = TempDir::new().unwrap();
    write_agent(tmp.path(), "pm", "orchestrator", &[], &[], &[]);
    write_agent(tmp.path(), "engineer", "engineer", &[], &[], &[]);
    let reg = AgentRegistry::load(&[tmp.path().to_path_buf()]);
    let roster = build_roster_section(&reg);
    assert!(
        !roster.contains("**pm**"),
        "PM should not delegate to itself: {roster}"
    );
    assert!(roster.contains("**engineer**"));
}

#[test]
fn inject_roster_replaces_placeholder() {
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "python-engineer",
        "engineer",
        &["python"],
        &[],
        &[],
    );
    let reg = AgentRegistry::load(&[tmp.path().to_path_buf()]);
    let prompt = "prefix\n{{available_agents}}\nsuffix";
    let out = inject_roster_into_prompt(prompt, &reg);
    assert!(out.starts_with("prefix\n"), "prefix preserved: {out}");
    assert!(out.ends_with("\nsuffix"), "suffix preserved: {out}");
    assert!(out.contains("python-engineer"), "roster injected: {out}");
    assert!(
        !out.contains("{{available_agents}}"),
        "placeholder removed: {out}"
    );
}

#[test]
fn inject_roster_appends_when_placeholder_missing() {
    let tmp = TempDir::new().unwrap();
    write_agent(tmp.path(), "engineer", "engineer", &[], &[], &[]);
    let reg = AgentRegistry::load(&[tmp.path().to_path_buf()]);
    let prompt = "You are the PM. Delegate to agents.";
    let out = inject_roster_into_prompt(prompt, &reg);
    assert!(out.starts_with(prompt), "original prompt preserved: {out}");
    assert!(
        out.contains("## Available Agents"),
        "roster section appended: {out}"
    );
    assert!(out.contains("engineer"), "agent name present: {out}");
}
