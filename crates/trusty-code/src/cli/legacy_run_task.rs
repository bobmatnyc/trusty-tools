//! `tcode run-task --legacy-in-process` — the ORIGINAL in-process execution
//! path (epic #1039 / #1034), split out of `main.rs` by #4434.
//!
//! Why: this is the one subcommand handler in this binary that is NOT a thin
//! JSON-RPC client — it runs the PM→engineer `AgentLoop` in THIS process by
//! calling `trusty_code::run_task::execute_run_task` directly, which forces
//! it to own three concerns no sibling handler has: agent-name validation
//! (the name is joined into a filesystem path), engineer-model resolution
//! (CLI flag > `TCODE_ENGINEER_MODEL` env > agent config), and construction
//! of the real dispatching LLM client. Those ~160 SLOC sat in `main.rs`,
//! which reached 498 of the mechanically-enforced 500-SLOC production cap
//! (`scripts/check_line_cap.sh`) after #4424 added `Command::Tui` — leaving
//! no room for the next feature to touch the file at all. Moving the whole
//! legacy block here (function, its two private helpers, and their tests as
//! one unit) restores `main.rs` to what its own docs claim it is: clap
//! definitions plus one-line dispatch. It lives under `crate::cli` with the
//! other subcommand handlers because it IS one; see [`super`]'s module docs
//! for why this module and `tui` are the two members that are not
//! JSON-RPC-over-stdio translators.
//! What: [`run`] is the binary-layer wrapper the `--legacy-in-process` flag
//! dispatches to, plus the two private helpers it alone uses —
//! `validate_agent_name` (path-traversal guard) and `build_llm_client`
//! (`DispatchingLlmClient` construction). The default (thin-client) `run-task`
//! path is the separate `super::run_task` module.
//! Test: `tests` in this module covers the helpers offline
//! (`build_llm_client_succeeds_without_openrouter_key`,
//! `validate_agent_name_rejects_traversal`,
//! `validate_agent_name_accepts_valid`,
//! `validate_agent_name_accepts_namespaced_plugin_agent`,
//! `validate_agent_name_rejects_namespaced_traversal`); the orchestration
//! [`run`] delegates to is covered by `trusty_code::run_task::tests`, which
//! exercises `execute_run_task` directly and is unaffected by this
//! CLI-layer move.

use std::path::Path;
use std::process;
use std::sync::Arc;

use anyhow::Result;
use tracing::info;

use trusty_code::llm::{DispatchingLlmClient, InferenceAdapter};
use trusty_code::plugins::is_valid_namespaced_name;
use trusty_code::run_task::{ExitCode, RunTaskParams, execute_run_task};

/// Environment variable that overrides the engineer model for a single run (#1035).
const ENGINEER_MODEL_ENV: &str = "TCODE_ENGINEER_MODEL";

/// Execute `tcode run-task`: run the PM→engineer pipeline and print the report.
///
/// Why: This is the binary-layer wrapper over the library orchestrator. It owns
/// only the concerns that are genuinely CLI-shaped — argument validation, env
/// key/model resolution, output mode, and process exit codes — and delegates all
/// orchestration to `trusty_code::run_task::execute_run_task` (which is fully
/// unit-tested offline). When `--json` is set, only the JSON report is written to
/// stdout; logs always go to stderr.
/// What: Validates the agent name and project path (traversal guards), locates the
/// agents dir, resolves the engineer-model override (CLI flag > `TCODE_ENGINEER_MODEL`
/// env), builds the dispatching LLM client (`build_llm_client`, OpenRouter and/or
/// Bedrock depending on what's configured and what the resolved model actually
/// needs — see #2245), runs `execute_run_task` (threading `--timeout-seconds`
/// (#2207) through as `RunTaskParams.deadline_secs` — final flag/env/default
/// resolution happens inside `execute_run_task` via `resolve_deadline_secs`),
/// prints the human or JSON report, and exits with the report's `ExitCode`. A
/// missing OpenRouter key is only a config error (exit 2) when the resolved
/// model actually needs OpenRouter; a pure-Bedrock model needs only AWS
/// credentials, surfaced (if absent) as a run failure from the first Bedrock call.
/// Test: Orchestration (incl. the model swap) is covered by `run_task::tests`; the
/// wrapper is exercised manually via `tcode run-task pm "<task>" --project <path>`.
pub async fn run(
    agent_name: &str,
    task: &str,
    project: &Path,
    json: bool,
    engineer_model_flag: Option<String>,
    timeout_seconds: Option<u64>,
) -> Result<()> {
    if let Err(e) = validate_agent_name(agent_name) {
        eprintln!("tcode run-task: {e}");
        process::exit(ExitCode::ConfigError.code());
    }

    // Resolve canonical project root (path-traversal-safe: canonicalize collapses
    // any `..` so the project root is a real, existing directory).
    let project_root = match project.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "tcode run-task: invalid --project path '{}': {e}",
                project.display()
            );
            process::exit(ExitCode::ConfigError.code());
        }
    };

    let agents_dir = trusty_code::agents::locate_agents_dir(&project_root);

    // Engineer-model override (#1035): CLI flag wins, then the env var; an empty
    // value is treated as "unset" so it falls through to the agent config model.
    let engineer_model = engineer_model_flag
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var(ENGINEER_MODEL_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
        });

    info!(
        agent = agent_name,
        project = %project_root.display(),
        agents_dir = %agents_dir.display(),
        json,
        engineer_model = engineer_model.as_deref().unwrap_or("(agent default)"),
        timeout_seconds = ?timeout_seconds,
        "tcode run-task: starting"
    );

    // Build the real OpenRouter client from env. A missing key is a config error.
    let llm: Arc<dyn InferenceAdapter> = match build_llm_client() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("tcode run-task: {e}");
            process::exit(ExitCode::ConfigError.code());
        }
    };

    let params = RunTaskParams {
        agent: agent_name.to_string(),
        task: task.to_string(),
        project: project_root,
        agents_dir,
        engineer_model,
        deadline_secs: timeout_seconds,
    };

    let report = execute_run_task(params, llm).await;

    // Print the report. In `--json` mode stdout carries only the JSON document.
    if json {
        println!("{}", report.render_json());
    } else {
        println!("{}", report.render_human());
    }

    process::exit(report.exit.code());
}

/// Validate that `agent_name` contains only safe filesystem characters —
/// either a plain name, or a `<plugin>:<name>` namespaced plugin agent
/// dispatch name (issue #3539/#3547).
///
/// Why: The agent name is joined into a filesystem path
/// (`<agents_dir>/<agent_name>.md`). Without this guard a crafted name such
/// as `../../etc/passwd` escapes the agents directory and enables path
/// traversal. Restricting to `[a-zA-Z0-9_-]` is safe, predictable, and covers
/// every real plain agent name in use. A namespaced plugin agent name
/// (`<plugin>:<name>`) previously had no valid shape at all here — any name
/// containing `:` was rejected outright, so a plugin agent could never be
/// dispatched via `tcode run-task` even though `agents::resolve_agent`
/// already resolves it (code-critic PR #3547 review, HIGH 4).
/// What: a namespaced shape is checked FIRST via
/// `trusty_code::plugins::is_valid_namespaced_name` (exactly two
/// `:`-separated segments, each `[a-z0-9-]+` and ≤64 chars — the SAME
/// charset `agents::protocol::validate_agent_name` already enforces for the
/// disk-catalog write path, reused rather than re-derived) and accepted
/// immediately if it matches; this is strictly stricter than the plain-name
/// charset below (no need to also run the looser check). Otherwise, returns
/// `Ok(())` when every character is ASCII alphanumeric, `_`, or `-`, and the
/// name is non-empty (the original plain-name contract, unchanged). `Err`
/// with a descriptive message otherwise.
/// Test: `validate_agent_name_rejects_traversal`,
/// `validate_agent_name_accepts_valid`,
/// `validate_agent_name_accepts_namespaced_plugin_agent`,
/// `validate_agent_name_rejects_namespaced_traversal` in this module.
fn validate_agent_name(agent_name: &str) -> Result<()> {
    if is_valid_namespaced_name(agent_name) {
        return Ok(());
    }
    if agent_name.is_empty()
        || !agent_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!(
            "invalid agent name '{agent_name}': agent names must be non-empty and contain \
             only [a-zA-Z0-9_-], or be a namespaced <plugin>:<name> where both segments are \
             [a-z0-9-]+ (<= 64 chars)"
        );
    }
    Ok(())
}

/// Build the real LLM client, dispatching `bedrock/*` model slugs to AWS
/// Bedrock, `fireworks/*` to Fireworks, and everything else to OpenRouter —
/// all via the shared `trusty_common::inference` adapter for the OpenAI-dialect
/// providers (#1021 phase 1; #2406 migration).
///
/// Why: `DispatchingLlmClient` is what lets `--engineer-model bedrock/us.anthropic.*`
/// (or `TCODE_ENGINEER_MODEL`) reach AWS Bedrock, `fireworks/*` reach Fireworks,
/// and every other slug reach OpenRouter — the caller keeps depending only on
/// `InferenceAdapter`. The Bedrock transport is constructed lazily on first use
/// (standard AWS credential chain, e.g. `AWS_PROFILE=cto`), so a pure-OpenRouter
/// run never needs AWS credentials. Credentials for the OpenAI-dialect providers
/// are resolved by the shared 3-tier chain (process env > `.env.local` > secure
/// store) at first use, not read here — so construction touches no secrets and
/// cannot fail on a missing key (#2245); a missing key surfaces as a clear error
/// the moment a model that needs it is actually dispatched
/// (`DispatchingLlmClient::chat`), never at startup.
/// What: constructs a `DispatchingLlmClient` (infallible — no credential access
/// at construction) and boxes it as the shared `Arc<dyn InferenceAdapter>`.
/// Test: `tests::build_llm_client_succeeds_without_openrouter_key`; a live
/// run against each backend is exercised manually.
fn build_llm_client() -> Result<Arc<dyn InferenceAdapter>> {
    Ok(Arc::new(DispatchingLlmClient::new()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{build_llm_client, validate_agent_name};

    /// `build_llm_client` must succeed even when `OPENROUTER_API_KEY` is unset
    /// — a pure-Bedrock run (`TCODE_ENGINEER_MODEL=bedrock/...`) must not be
    /// blocked by a missing OpenRouter key it will never use (#2245).
    ///
    /// Why: Pins the exact bug reported by the live smoke test: construction
    /// used to hard-fail with "OPENROUTER_API_KEY is required" regardless of
    /// which model was actually being targeted.
    /// What: Temporarily unsets `OPENROUTER_API_KEY` (restoring it afterwards
    /// even on panic-free assertion failure — the restore runs before the
    /// assert), calls `build_llm_client`, asserts `Ok`.
    /// Test: this test. No other test in this binary touches
    /// `OPENROUTER_API_KEY`, so this is safe under `cargo test`'s default
    /// parallel test execution.
    #[test]
    fn build_llm_client_succeeds_without_openrouter_key() {
        let prev = std::env::var("OPENROUTER_API_KEY").ok();
        // SAFETY: test-only env mutation; no other test in this binary reads
        // or writes `OPENROUTER_API_KEY`.
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
        let result = build_llm_client();
        if let Some(key) = prev {
            // SAFETY: see above.
            unsafe {
                std::env::set_var("OPENROUTER_API_KEY", key);
            }
        }
        assert!(
            result.is_ok(),
            "expected Ok when OPENROUTER_API_KEY is unset (pure-Bedrock runs must not \
             be blocked by it), got err: {:?}",
            result.err()
        );
    }

    /// A path-traversal agent name is rejected.
    ///
    /// Why: Guards the `agents_dir.join(format!("{agent_name}.md"))` path
    /// construction in the run-task pipeline against names that escape the agents
    /// directory.
    /// What: Asserts that `../../etc/passwd` and similar strings fail validation.
    /// Test: This test.
    #[test]
    fn validate_agent_name_rejects_traversal() {
        assert!(
            validate_agent_name("../../etc/passwd").is_err(),
            "path traversal must be rejected"
        );
        assert!(
            validate_agent_name("../sibling").is_err(),
            "parent-dir component must be rejected"
        );
        assert!(
            validate_agent_name("agent/subdir").is_err(),
            "path separator must be rejected"
        );
        assert!(
            validate_agent_name("agent name").is_err(),
            "space must be rejected"
        );
        assert!(
            validate_agent_name("").is_err(),
            "empty string must be rejected"
        );
        assert!(
            validate_agent_name("agent\0null").is_err(),
            "null byte must be rejected"
        );
    }

    /// A well-formed agent name is accepted.
    ///
    /// Why: Verifies the allowlist does not over-reject legitimate names.
    /// What: Asserts that common agent names (`pm`, `qa-agent`, etc.) pass.
    /// Test: This test.
    #[test]
    fn validate_agent_name_accepts_valid() {
        assert!(validate_agent_name("pm").is_ok());
        assert!(validate_agent_name("engineer").is_ok());
        assert!(validate_agent_name("qa-agent").is_ok());
        assert!(validate_agent_name("python_engineer").is_ok());
        assert!(validate_agent_name("rust-engineer-2024").is_ok());
        assert!(
            validate_agent_name("A").is_ok(),
            "single ASCII letter must pass"
        );
    }

    /// A well-formed `<plugin>:<name>` namespaced plugin agent dispatch
    /// name is accepted (issue #3547 HIGH 4 — this is what actually enables
    /// `tcode run-task <plugin>:<name>` to reach `resolve_agent` at all;
    /// previously every namespaced name was rejected here before `run-task`
    /// ever looked up an agent).
    ///
    /// Test: this test.
    #[test]
    fn validate_agent_name_accepts_namespaced_plugin_agent() {
        assert!(validate_agent_name("my-plugin:reviewer").is_ok());
        assert!(validate_agent_name("a:b").is_ok());
    }

    /// A traversal or malformed payload disguised as a namespaced name is
    /// still rejected (issue #3547 HIGH 4 — accepting the `<plugin>:<name>`
    /// shape must not reopen the traversal guard it sits next to).
    ///
    /// Test: this test.
    #[test]
    fn validate_agent_name_rejects_namespaced_traversal() {
        assert!(validate_agent_name("my-plugin:../../etc/passwd").is_err());
        assert!(validate_agent_name("../../etc:reviewer").is_err());
        assert!(
            validate_agent_name("my-plugin:a:b").is_err(),
            "extra colon must be rejected"
        );
    }
}
