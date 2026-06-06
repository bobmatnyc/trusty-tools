//! PM harness inspection mode (#harness-test-suite).
//!
//! Why: Validating that the PM picks the right agent/skills/tools for a given
//! task description requires two separate test layers. Layer 1 (static)
//! answers "does the registry routing work?" without any LLM cost —
//! task text → keyword signals → `AgentRegistry::best_match`. Layer 2 (live)
//! runs the PM through one LLM turn and captures the `delegate_to_agent`
//! tool call. This module owns the Layer 1 primitives and the `inspect`
//! subcommand entry point; Layer 2 is a future extension.
//! What: `TaskSignals::extract` turns task text into language/framework/
//! role/tag signals via keyword matching. `run_inspect_dry_run` wires those
//! signals through the `AgentRegistry` + `SkillRegistry` and prints a JSON
//! report to stdout. A live (non dry-run) mode is planned but returns an
//! explanatory error today so CI can gate on dry-run behaviour.
//! Test: Unit tests in `task_signals.rs`; end-to-end coverage via
//! `tests/harness/run_inspection.sh` and static assertions in the tests
//! module at the bottom of `task_signals.rs`.

pub mod task_signals;

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::agents::registry::{AgentRegistry, agent_search_paths};
use crate::skills::registry::{SkillRegistry, skill_search_paths};

pub use task_signals::TaskSignals;

/// Execute `trusty-agents inspect --task <text> [--dry-run]`.
///
/// Why: Gives operators (and the harness test suite) a single command that
/// reports which agent + skills would be chosen for a given task. Dry-run
/// mode is the default — it extracts signals and queries the registries
/// with zero LLM cost, which is what CI gates on today.
/// What: In dry-run mode, prints a pretty-printed JSON doc containing the
/// raw signals plus the registry's `best_match` and top-5 tag-matched
/// skills. Live mode (no `--dry-run`) is a future extension that would
/// run one PM chat turn and capture the `delegate_to_agent` tool call;
/// today it returns an error so callers know to use `--dry-run`.
/// Test: `inspect_dry_run_python_task_picks_python_engineer` and
/// `inspect_dry_run_emits_expected_json_shape`.
pub async fn run_inspect_subcommand(args: &[String]) -> Result<()> {
    let mut task: Option<String> = None;
    let mut task_file: Option<String> = None;
    let mut dry_run = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--task" => {
                task = Some(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow::anyhow!("--task requires a value"))?
                        .clone(),
                );
                i += 2;
            }
            "--task-file" => {
                task_file = Some(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow::anyhow!("--task-file requires a value"))?
                        .clone(),
                );
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            other => {
                bail!(
                    "tagent inspect: unknown argument '{other}'. \
                     Usage: inspect --task <text> [--dry-run]"
                );
            }
        }
    }

    let task_text = match (task, task_file) {
        (Some(t), _) => t,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read --task-file {path}: {e}"))?,
        (None, None) => bail!("tagent inspect requires --task <text> or --task-file <path>"),
    };

    let config_dir = crate::default_bundled_config_dir();

    if dry_run {
        run_inspect_dry_run(&task_text, &config_dir, true)
    } else {
        let agent_registry = AgentRegistry::load(&agent_search_paths(&config_dir));
        let skill_registry = SkillRegistry::load(&skill_search_paths(&config_dir));
        run_inspect_live(&task_text, &agent_registry, &skill_registry).await
    }
}

/// Live inspection: runs one PM LLM turn with the real `delegate_to_agent`
/// tool schema, captures the tool-call decision, and prints JSON WITHOUT
/// actually spawning a sub-agent.
///
/// Why: Layer 2 of the harness — validates that the PM's LLM-side reasoning
/// picks the same agent the registry would (Layer 1 static prediction).
/// When live and static diverge, the JSON report makes it visible to
/// the operator / CI without executing the delegation (no sub-agent
/// subprocess, no cascading LLM cost).
/// What: Loads `pm.toml`, builds the delegate tool schema via
/// `crate::tools::delegate_to_agent_tool`, calls `llm::chat` with the
/// task as the user message, extracts the first tool_call's `agent_name`
/// + `task` args, and emits a JSON doc with both `live_decision` and
/// `static_prediction` for comparison. Requires `OPENROUTER_API_KEY`
/// (or `ANTHROPIC_API_KEY` depending on pm.toml); fails with a clear
/// error when the key is missing.
/// Test: Shell-level — `tests/harness/run_inspection.sh --live` (requires
/// API key). Not a cargo unit test because it hits a real LLM endpoint.
pub async fn run_inspect_live(
    task: &str,
    agent_registry: &AgentRegistry,
    skill_registry: &SkillRegistry,
) -> Result<()> {
    use crate::AgentConfig;
    use crate::llm;

    // Load PM config — mirror run_pm() so we exercise the same config path.
    let mut pm_cfg = AgentConfig::by_name("pm")
        .context("failed to load pm agent config — is .trusty-agents/agents/pm.toml present?")?;

    // Inject the dynamic agent roster — same policy as run_pm(). Mirroring
    // this here is critical: without it, Layer 2 (live) would test a
    // different prompt than production and mask the over-delegation bug.
    pm_cfg.system_prompt.content = crate::agents::registry::inject_roster_into_prompt(
        &pm_cfg.system_prompt.content,
        agent_registry,
    );

    let client = llm::create_client()
        .context("failed to create LLM client — is OPENROUTER_API_KEY set in .env.local?")?;

    // Build the delegate_to_agent tool schema.
    let delegate_tool = crate::tools::delegate_to_agent_tool()
        .context("failed to build delegate_to_agent tool schema")?;

    // Run ONE PM LLM turn — no execution of tool calls.
    let response = llm::chat(
        &client,
        &pm_cfg.agent.model,
        &pm_cfg.system_prompt.content,
        task,
        pm_cfg.llm.temperature,
        pm_cfg.llm.max_tokens,
        vec![delegate_tool],
    )
    .await
    .context("PM LLM call failed")?;

    // Extract the delegation decision.
    let decision = if let Some(tc) = response.tool_calls.first() {
        let agent_name = tc
            .arguments
            .get("agent_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let task_for_agent = tc
            .arguments
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        json!({
            "tool": tc.name,
            "agent": agent_name,
            "task_for_agent": task_for_agent,
        })
    } else {
        let text = response.content.clone().unwrap_or_default();
        json!({
            "tool": serde_json::Value::Null,
            "agent": serde_json::Value::Null,
            "pm_text_response": text,
            "note": "PM did not call delegate_to_agent — it may have answered directly"
        })
    };

    // Run static (Layer 1) prediction for side-by-side comparison.
    let signals = TaskSignals::extract(task);
    let lang_refs: Vec<&str> = signals.languages.iter().map(String::as_str).collect();
    let fw_refs: Vec<&str> = signals.frameworks.iter().map(String::as_str).collect();
    let tag_refs: Vec<&str> = signals.tags.iter().map(String::as_str).collect();
    let static_best =
        agent_registry.best_match(signals.role.as_deref(), &lang_refs, &fw_refs, &tag_refs);

    let mut all_tags: Vec<&str> = Vec::new();
    all_tags.extend(lang_refs.iter().copied());
    all_tags.extend(fw_refs.iter().copied());
    all_tags.extend(tag_refs.iter().copied());
    let matched_skills: Vec<String> = skill_registry
        .find_by_tags(&all_tags)
        .iter()
        .take(5)
        .map(|s| s.name.clone())
        .collect();

    let live_agent: Option<String> = decision
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let matches_static = live_agent.as_deref() == static_best;

    let output = json!({
        "task": task,
        "mode": "live",
        "live_decision": decision,
        "static_prediction": {
            "best_match": static_best,
            "signals": {
                "languages": signals.languages,
                "frameworks": signals.frameworks,
                "role": signals.role,
                "tags": signals.tags,
            },
            "matched_skills": matched_skills,
        },
        "validation": {
            "live_matches_static": matches_static,
            "live_agent": live_agent,
            "static_agent": static_best,
        }
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Load the registries and print the inspection JSON report.
///
/// Why: Split out from the CLI wrapper so unit tests can call it without
/// fabricating argv. Also makes the "live mode not implemented" branch
/// testable.
/// What: Scans `agent_search_paths` / `skill_search_paths`, extracts task
/// signals, runs `best_match` + `find_by_tags`, and prints the JSON to
/// stdout. Non dry-run returns an error explaining to use `--dry-run`.
/// Test: `inspect_dry_run_python_task_picks_python_engineer`.
pub fn run_inspect_dry_run(task: &str, config_dir: &Path, dry_run: bool) -> Result<()> {
    let agent_registry = AgentRegistry::load(&agent_search_paths(config_dir));
    let skill_registry = SkillRegistry::load(&skill_search_paths(config_dir));

    let report = inspect_report(task, &agent_registry, &skill_registry);
    println!("{}", serde_json::to_string_pretty(&report)?);

    if !dry_run {
        // Callers wanting live mode should go through run_inspect_live;
        // this function is dry-run only. Preserve legacy error for tests
        // that may still call this directly.
        bail!("run_inspect_dry_run called with dry_run=false; use run_inspect_live instead");
    }
    Ok(())
}

/// Build the JSON inspection report for `task` against the given registries.
///
/// Why: Pure function (no I/O) so it's trivially unit-testable and reusable
/// by the future live-mode path which wants the same JSON shape.
/// What: Extracts signals, queries `best_match` + `find_by_tags(top 5)`,
/// and emits a `serde_json::Value` with `task`, `signals`, `registry`.
/// Test: `report_shape_contains_expected_keys`.
pub fn inspect_report(
    task: &str,
    agents: &AgentRegistry,
    skills: &SkillRegistry,
) -> serde_json::Value {
    let signals = TaskSignals::extract(task);
    let lang_refs: Vec<&str> = signals.languages.iter().map(String::as_str).collect();
    let fw_refs: Vec<&str> = signals.frameworks.iter().map(String::as_str).collect();
    let tag_refs: Vec<&str> = signals.tags.iter().map(String::as_str).collect();

    let best_match = agents.best_match(signals.role.as_deref(), &lang_refs, &fw_refs, &tag_refs);

    // All signals collapsed into a single tag vector for skill lookup.
    let mut all_tags: Vec<&str> = Vec::new();
    all_tags.extend(lang_refs.iter().copied());
    all_tags.extend(fw_refs.iter().copied());
    all_tags.extend(tag_refs.iter().copied());
    let matched_skills: Vec<String> = skills
        .find_by_tags(&all_tags)
        .iter()
        .take(5)
        .map(|s| s.name.clone())
        .collect();

    let available_agents: Vec<String> = agents.list().into_iter().map(|a| a.name.clone()).collect();

    json!({
        "task": task,
        "signals": {
            "languages": signals.languages,
            "frameworks": signals.frameworks,
            "role": signals.role,
            "tags": signals.tags,
        },
        "registry": {
            "best_match": best_match,
            "matched_skills": matched_skills,
            "available_agents": available_agents,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Path to the repo-bundled `.trusty-agents` directory (agents + skills).
    fn bundled_config_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".trusty-agents")
    }

    fn load_bundled_registries() -> (AgentRegistry, SkillRegistry) {
        let dir = bundled_config_dir();
        (
            AgentRegistry::load(&agent_search_paths(&dir)),
            SkillRegistry::load(&skill_search_paths(&dir)),
        )
    }

    #[test]
    fn report_shape_contains_expected_keys() {
        let (agents, skills) = load_bundled_registries();
        let v = inspect_report("Write a Python script", &agents, &skills);
        assert!(v.get("task").is_some(), "task key present");
        assert!(v.get("signals").is_some(), "signals key present");
        let reg = v.get("registry").expect("registry key present");
        assert!(reg.get("best_match").is_some());
        assert!(reg.get("matched_skills").is_some());
        assert!(reg.get("available_agents").is_some());
    }

    // ── Layer 1: static harness selection tests ────────────────────────────
    //
    // Why: Validate the registry selection logic against the bundled agent
    // catalog without spending any LLM cycles. Each test corresponds to one
    // entry in `.trusty-agents/tasks/harness-test-suite.toml`.

    #[test]
    fn python_csv_task_selects_python_engineer() {
        let (agents, _skills) = load_bundled_registries();
        let signals = TaskSignals::extract(
            "Write a Python script that reads a CSV file and outputs a JSON summary",
        );
        let lang_refs: Vec<&str> = signals.languages.iter().map(String::as_str).collect();
        let fw_refs: Vec<&str> = signals.frameworks.iter().map(String::as_str).collect();
        let tag_refs: Vec<&str> = signals.tags.iter().map(String::as_str).collect();
        let agent = agents.best_match(signals.role.as_deref(), &lang_refs, &fw_refs, &tag_refs);
        assert_eq!(
            agent,
            Some("python-engineer"),
            "Python CSV task should select python-engineer"
        );
    }

    #[test]
    fn research_task_selects_research_agent() {
        let (agents, _skills) = load_bundled_registries();
        let signals = TaskSignals::extract(
            "Check if https://httpbin.org/get is accessible and report the status",
        );
        let agent = agents.best_match(signals.role.as_deref(), &[], &[], &[]);
        assert_eq!(agent, Some("research-agent"));
    }

    #[test]
    fn docs_task_selects_docs_agent() {
        let (agents, _skills) = load_bundled_registries();
        let signals = TaskSignals::extract(
            "Write API documentation for a REST API with user management endpoints",
        );
        let agent = agents.best_match(signals.role.as_deref(), &[], &[], &[]);
        assert_eq!(agent, Some("docs-agent"));
    }

    #[test]
    fn bash_script_task_selects_local_ops() {
        let (agents, _skills) = load_bundled_registries();
        let signals = TaskSignals::extract(
            "Write a bash script that backs up a directory to a tar.gz archive",
        );
        let tag_refs: Vec<&str> = signals.tags.iter().map(String::as_str).collect();
        let agent = agents.best_match(signals.role.as_deref(), &[], &[], &tag_refs);
        assert_eq!(agent, Some("local-ops-agent"));
    }

    #[test]
    fn fastapi_task_loads_correct_skills() {
        let (_agents, skills) = load_bundled_registries();
        let results = skills.find_by_tags(&["fastapi", "pytest", "python"]);
        assert!(
            results.iter().any(|s| s.name == "fastapi"),
            "fastapi skill must be discoverable"
        );
        assert!(
            results.iter().any(|s| s.name == "pytest"),
            "pytest skill must be discoverable"
        );
        // Top-ranked result should carry >=2 of the queried tags; ties are
        // broken by discovery order so any python/pytest/fastapi skill with
        // ≥2 overlapping tags is acceptable.
        let first = results[0];
        let overlap = ["fastapi", "pytest", "python"]
            .iter()
            .filter(|q| first.tags.iter().any(|t| t.eq_ignore_ascii_case(q)))
            .count();
        assert!(
            overlap >= 2,
            "Top skill {} should carry >=2 queried tags (got {overlap})",
            first.name
        );
    }

    #[test]
    fn plan_task_selects_plan_agent() {
        let (agents, _skills) = load_bundled_registries();
        let signals = TaskSignals::extract(
            "Plan a multi-file Python project: a CLI weather app with API client and tests",
        );
        let agent = agents.best_match(signals.role.as_deref(), &[], &[], &[]);
        assert_eq!(agent, Some("plan-agent"));
    }

    #[test]
    fn qa_task_selects_qa_agent() {
        let (agents, _skills) = load_bundled_registries();
        let signals = TaskSignals::extract(
            "Run the pytest suite in ./out/weather-app/ and report failing tests",
        );
        let agent = agents.best_match(signals.role.as_deref(), &[], &[], &[]);
        assert_eq!(agent, Some("qa-agent"));
    }
}
