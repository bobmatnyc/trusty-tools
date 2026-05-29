//! Python subprocess tool plugin (#446).
//!
//! Why: Agents need to expose domain-specific tooling (custom reports, data
//! lookups, internal APIs) without baking that logic into the open-mpm core
//! binary. A subprocess plugin lets operators ship a Python script alongside
//! their agent TOML — the harness spawns `python3 <script>`, hands it
//! JSON-encoded parameters on stdin, and reads back a structured result on
//! stdout. Same NDJSON envelope used elsewhere in the harness.
//! What: `PythonToolPlugin` holds the static metadata (name, schema, script
//! path, timeout, RBAC tier guard) parsed from `[[plugins.python]]` agent TOML
//! entries. `execute(params)` spawns one short-lived `python3` child per call,
//! writes a single `{"type":"tool_call",...}` line, reads back one
//! `{"type":"tool_result",...}` line, and returns a `ToolResult`. Timeouts kill
//! the child; non-zero exits and malformed JSON become recoverable errors.
//! Test: See `tests` at the bottom of this file — happy path uses an inline
//! `python3 -c "..."` script that echoes a fixed result; timeout, non-zero
//! exit, and malformed stdout all produce structured `ToolResult::Error`s.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::tools::traits::{ToolExecutor, ToolResult};

/// Default per-call timeout (seconds) when the agent TOML omits `timeout_secs`.
///
/// Why: 30s is generous enough for I/O-bound scripts (HTTP fetches, DB
/// queries) without letting a runaway child block the agent loop forever.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Raw TOML shape for one `[[plugins.python]]` entry.
///
/// Why: Kept distinct from the runtime `PythonToolPlugin` so we can parse the
/// loose, file-shaped form (either `schema_file` or inline `schema`) and then
/// normalise it into a single owned `Value` schema for execution.
/// What: All fields except `name`, `description`, and `script` have defaults.
/// `schema` and `schema_file` are mutually optional — at least one should be
/// supplied; both missing yields an empty object schema.
/// Test: See `python_plugin_config_parses_inline_schema` and
/// `python_plugin_config_parses_schema_file`.
#[derive(Debug, Clone, Deserialize)]
pub struct PythonPluginConfig {
    pub name: String,
    pub description: String,
    pub script: PathBuf,
    /// JSON Schema for the tool's `parameters`, supplied inline in TOML.
    #[serde(default)]
    pub schema: Option<Value>,
    /// Path to a JSON file containing the parameters schema. Resolved relative
    /// to the agent TOML directory by `PythonToolPlugin::from_config`.
    #[serde(default)]
    pub schema_file: Option<PathBuf>,
    /// Per-call timeout in seconds. Defaults to `DEFAULT_TIMEOUT_SECS`.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// RBAC tier guard (#446). Empty list = unrestricted. When a caller's
    /// effective tier is in this list, the plugin refuses to execute.
    #[serde(default)]
    pub restricted_tiers: Vec<String>,
}

/// Runtime-ready Python subprocess tool.
///
/// Why: Splits the loose TOML config (`PythonPluginConfig`) from a normalised
/// runtime value with a resolved schema and absolute script path. This way the
/// hot path (`execute`) never has to redo IO that should happen once at load.
/// What: Fields are the minimum the executor needs — name, description, script
/// path, schema, timeout, and RBAC tier guard.
/// Test: See `tests` below — `from_config` resolves schemas; `execute` covers
/// the success / timeout / exit / parse-error paths.
#[derive(Debug, Clone)]
pub struct PythonToolPlugin {
    pub name: String,
    pub description: String,
    pub script: PathBuf,
    pub schema: Value,
    pub timeout: Duration,
    pub restricted_tiers: Vec<String>,
}

impl PythonToolPlugin {
    /// Build a runtime plugin from a TOML config plus the agent TOML's
    /// directory (used to resolve relative `script` / `schema_file` paths).
    ///
    /// Why: Relative paths in `[[plugins.python]]` are intuitively relative to
    /// the agent TOML, not the process CWD. Resolving them once at load time
    /// avoids surprising "file not found" errors deep in agent execution.
    /// What: Joins `base_dir` with `script` (and `schema_file` if present) when
    /// the path is relative; reads the schema file when supplied; falls back to
    /// inline `schema`, then to an empty object schema if neither is set.
    /// Test: `python_tool_plugin_from_config_resolves_relative_paths`,
    /// `python_tool_plugin_from_config_loads_schema_file`.
    pub fn from_config(cfg: PythonPluginConfig, base_dir: &Path) -> anyhow::Result<Self> {
        let script = if cfg.script.is_absolute() {
            cfg.script
        } else {
            base_dir.join(&cfg.script)
        };

        let schema = match (cfg.schema, cfg.schema_file) {
            (Some(inline), _) => inline,
            (None, Some(path)) => {
                let resolved = if path.is_absolute() {
                    path
                } else {
                    base_dir.join(&path)
                };
                let raw = std::fs::read_to_string(&resolved).map_err(|e| {
                    anyhow::anyhow!(
                        "python plugin '{}': failed to read schema_file {}: {}",
                        cfg.name,
                        resolved.display(),
                        e
                    )
                })?;
                serde_json::from_str(&raw).map_err(|e| {
                    anyhow::anyhow!(
                        "python plugin '{}': schema_file {} is not valid JSON: {}",
                        cfg.name,
                        resolved.display(),
                        e
                    )
                })?
            }
            (None, None) => json!({"type": "object", "properties": {}}),
        };

        let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

        Ok(Self {
            name: cfg.name,
            description: cfg.description,
            script,
            schema,
            timeout,
            restricted_tiers: cfg.restricted_tiers,
        })
    }

    /// Render this plugin as an OpenAI-compatible tool spec (the `{"type":
    /// "function", ...}` envelope the LLM sees).
    ///
    /// Why: Plugin tools must look indistinguishable from native tools at the
    /// wire level so the LLM can call them with the same JSON tool-call shape.
    /// What: Wraps `description` + `schema` in the standard envelope. Clones
    /// the schema so callers can keep using `&self`.
    /// Test: `python_tool_plugin_to_tool_spec_shape`.
    pub fn to_tool_spec(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.schema,
            }
        })
    }

    /// Whether a caller with the given tier is forbidden from invoking this
    /// plugin (RBAC guard, #446).
    ///
    /// Why: Some plugins expose sensitive data (priority email, financials)
    /// and should not be visible to lower-privilege tiers. Returning the guard
    /// decision as a separate method lets the dispatch layer choose between
    /// "hide from tool list" and "return error on invocation".
    /// What: True when `tier` matches any string in `restricted_tiers`. Empty
    /// `restricted_tiers` => always returns false.
    /// Test: `python_tool_plugin_rbac_guard`.
    pub fn is_restricted_for_tier(&self, tier: &str) -> bool {
        self.restricted_tiers.iter().any(|t| t == tier)
    }

    /// Execute the plugin with the given JSON parameters.
    ///
    /// Why: This is the hot path — every LLM tool call lands here. We isolate
    /// the spawn / write / read / kill logic so a single helper handles all
    /// failure modes uniformly.
    /// What: Spawns `python3 <script>`, writes one NDJSON `tool_call` line to
    /// stdin, closes stdin, reads one NDJSON line from stdout, and returns the
    /// parsed `content`. Timeout, non-zero exit, missing stdout, and malformed
    /// JSON all become recoverable `ToolResult::Error`s.
    /// Test: `execute_happy_path_inline_python`, `execute_returns_timeout_error`,
    /// `execute_returns_error_on_nonzero_exit`, `execute_returns_error_on_malformed_json`.
    pub async fn execute(&self, params: Value) -> ToolResult {
        match self.execute_inner(params).await {
            Ok(result) => result,
            Err(e) => ToolResult::err(format!("python plugin '{}' failed: {}", self.name, e)),
        }
    }

    async fn execute_inner(&self, params: Value) -> anyhow::Result<ToolResult> {
        let id = Uuid::new_v4().to_string();
        let call = json!({
            "type": "tool_call",
            "id": id,
            "params": params,
        });
        let line = format!("{}\n", serde_json::to_string(&call)?);

        // Allow tests / advanced users to override the interpreter (e.g. set
        // `OPEN_MPM_PYTHON=python` on systems where only `python` is on PATH).
        let python = std::env::var("OPEN_MPM_PYTHON").unwrap_or_else(|_| "python3".to_string());

        // For the special "-c" + inline-script test pattern we accept a script
        // path whose first component is literally `-c:` followed by the inline
        // body. This keeps the test surface honest (no temp files) without
        // introducing a separate "inline" config field for production use.
        let script_str = self.script.to_string_lossy().to_string();
        let mut cmd = Command::new(&python);
        if let Some(inline) = script_str.strip_prefix("-c:") {
            cmd.arg("-c").arg(inline);
        } else {
            cmd.arg(&self.script);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        debug!(plugin = %self.name, script = %script_str, "spawning python plugin");
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn `{} {}`: {}", python, script_str, e))?;

        // Write the single tool_call line, then close stdin so the child sees EOF.
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
            drop(stdin);
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("child stdout missing"))?;
        let mut reader = BufReader::new(stdout).lines();

        // Read one line + wait for exit, all under a single timeout.
        let work = async {
            let line_opt = reader.next_line().await?;
            let status = child.wait().await?;
            Ok::<_, anyhow::Error>((line_opt, status))
        };

        let (line_opt, status) = match timeout(self.timeout, work).await {
            Ok(res) => res?,
            Err(_) => {
                // Best-effort kill; reader / child drop will reap.
                warn!(plugin = %self.name, "python plugin timed out after {:?}", self.timeout);
                return Ok(ToolResult::err(format!(
                    "python plugin '{}' timed out after {}s",
                    self.name,
                    self.timeout.as_secs()
                )));
            }
        };

        if !status.success() {
            return Ok(ToolResult::err(format!(
                "python plugin '{}' exited with status {:?}",
                self.name,
                status.code()
            )));
        }

        let Some(line) = line_opt else {
            return Ok(ToolResult::err(format!(
                "python plugin '{}' produced no stdout",
                self.name
            )));
        };

        // Parse the NDJSON result envelope.
        let parsed: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "python plugin '{}' returned malformed JSON: {} (line: {})",
                    self.name,
                    e,
                    line.trim()
                )));
            }
        };

        let status_field = parsed
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("success");
        let content = parsed
            .get("content")
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default();

        if status_field == "error" {
            let err_msg = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or(&content);
            return Ok(ToolResult::err(format!(
                "python plugin '{}' reported error: {}",
                self.name, err_msg
            )));
        }

        Ok(ToolResult::ok(content))
    }
}

#[async_trait]
impl ToolExecutor for PythonToolPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        self.to_tool_spec()
    }

    async fn execute(&self, args: Value) -> ToolResult {
        PythonToolPlugin::execute(self, args).await
    }
}

#[cfg(test)]
mod tests;
