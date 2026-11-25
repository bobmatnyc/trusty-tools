//! The agent job kind: `claude -p --agent <name>` from a scratch directory.
//!
//! Why: an agent run costs tokens and must never see the repo root, MCP
//! servers or slash commands (contract section 2, item 3).
//! What: [`plan`] renders the prompt (`{date}` is the UTC date) into a fresh
//! scratch directory and builds the [`RunSpec`]: argv with `--model`,
//! `--tools`, `--disable-slash-commands`, `--strict-mcp-config`, an empty
//! `--mcp-config` and `--output-format json`; cwd is the scratch directory,
//! stdin is the prompt file, stdout goes to a file there. [`finish`] reads the
//! JSON result, writes its text to `output_file` (when set and the run was
//! ok), appends `{ts, job, model, in, out, ok}` to the usage log, and removes
//! the scratch directory. Nothing here starts a process; the runner is
//! injected, so no test spends a token.
//! Test: `agent_plan_builds_the_contract_argv`, `agent_finish_writes_output_and_usage`,
//! `agent_finish_writes_nothing_on_failure`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::config::{AgentSpec, Job};
use super::runlog::append_jsonl;
use super::slot::day_key;
use super::spawn::{RunResult, RunSpec};
use super::{Layout, SchedError};
use crate::classify::alerts::iso;

/// A prepared agent run.
#[derive(Debug, Clone)]
pub struct AgentPlan {
    /// What to run.
    pub spec: RunSpec,
    /// The scratch directory, removed by [`finish`].
    pub scratch: PathBuf,
    /// The model, for the usage record.
    pub model: String,
    /// Where the result text goes, when set.
    pub output_file: Option<PathBuf>,
}

#[derive(Serialize)]
struct Usage<'a> {
    ts: String,
    job: &'a str,
    model: &'a str,
    #[serde(rename = "in")]
    tokens_in: u64,
    out: u64,
    ok: bool,
}

/// Prepares the run for `job` (an agent job) at `now`.
///
/// # Errors
/// [`SchedError::Io`] when the scratch directory or prompt cannot be written;
/// [`SchedError::Usage`] when `job` has no agent.
pub fn plan(
    job: &Job,
    layout: &Layout,
    scratch_root: &Path,
    env: Vec<(String, String)>,
    now: i64,
) -> Result<AgentPlan, SchedError> {
    let a: &AgentSpec = job
        .agent
        .as_ref()
        .ok_or_else(|| SchedError::Usage(format!("{}: not an agent job", job.id)))?;
    let date = day_key(now);
    let scratch = scratch_root.join(format!("{}-{now}", job.id));
    std::fs::create_dir_all(&scratch)
        .map_err(|e| SchedError::io(format!("create {}", scratch.display()), e))?;
    let prompt = a
        .prompt
        .clone()
        .unwrap_or_default()
        .replace("{date}", &date);
    let prompt_file = scratch.join("prompt.txt");
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| SchedError::io(format!("write {}", prompt_file.display()), e))?;
    let argv = [
        "claude",
        "-p",
        "--agent",
        &a.name,
        "--model",
        &a.model,
        "--tools",
        &a.tools.join(","),
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--mcp-config={\"mcpServers\":{}}",
        "--output-format",
        "json",
    ]
    .map(str::to_owned)
    .to_vec();
    let log = layout.job_log(&job.id, &date);
    Ok(AgentPlan {
        spec: RunSpec {
            argv,
            cwd: scratch.clone(),
            env,
            stdin: Some(prompt_file),
            stdout: scratch.join("out.json"),
            stderr: log,
            timeout: Duration::from_secs(job.timeout()),
        },
        model: a.model.clone(),
        output_file: a
            .output_file
            .as_ref()
            .map(|f| layout.root.join(f.replace("{date}", &date))),
        scratch,
    })
}

/// Records an agent run's outcome and removes the scratch directory.
///
/// # Errors
/// [`SchedError::Io`] when the output file or usage log cannot be written.
pub fn finish(
    plan: &AgentPlan,
    job: &str,
    result: &RunResult,
    layout: &Layout,
    now: i64,
) -> Result<(), SchedError> {
    let ok = result.exit == Some(0) && !result.timed_out && result.error.is_none();
    let doc: serde_json::Value = std::fs::read_to_string(&plan.spec.stdout)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null);
    let tokens = |k: &str| doc["usage"][k].as_u64().unwrap_or(0);
    let written = match (&plan.output_file, doc["result"].as_str()) {
        (Some(file), Some(text)) if ok => {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| SchedError::io(format!("create {}", dir.display()), e))?;
            }
            std::fs::write(file, text)
                .map_err(|e| SchedError::io(format!("write {}", file.display()), e))?;
            true
        }
        _ => false,
    };
    let usage = Usage {
        ts: iso(now as f64),
        job,
        model: &plan.model,
        tokens_in: tokens("input_tokens"),
        out: tokens("output_tokens"),
        ok: ok && (plan.output_file.is_none() || written),
    };
    let logged = append_jsonl(&layout.usage(), &usage);
    // A leftover scratch directory is harmless; the usage record is not.
    let _ = std::fs::remove_dir_all(&plan.scratch);
    logged
}
