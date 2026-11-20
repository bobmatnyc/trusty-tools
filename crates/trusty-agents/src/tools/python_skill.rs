//! Generic bridge: turns a Python skill directory's `manifest.json` into
//! registered `ToolExecutor`s, with zero per-skill Rust code.
//!
//! Why: a persona's `[tools].allow` can name tools that nothing in the live
//!      dispatch path implements (#3700). Writing each one as Rust would
//!      re-embed persona-specific business logic in this crate, which #3656
//!      objects to (DOC-41 §2.0 "declarative-only" agents). The business
//!      logic lives in a *skill* instead (Python code + data, bundled
//!      together). This module is the generic seam that makes that possible:
//!      it knows nothing about any one skill, only how to read a skill
//!      manifest and run it.
//! What: [`SkillManifest`] is the on-disk `manifest.json` shape (`persona`,
//!       `python.dir` + `python.command`, and a `tools[]` list of
//!       name/description/input_schema). [`PythonSkillToolExecutor`]
//!       implements `ToolExecutor` by spawning `python.command` (from
//!       `python.dir` as the working directory) with the tool name appended
//!       as an extra argument, writing the call's JSON arguments to stdin,
//!       and parsing exactly one JSON object off stdout — a top-level
//!       `"error"` key becomes a recoverable `ToolResult::err`, anything
//!       else becomes `ToolResult::ok` with the JSON re-serialised.
//!       [`build_plugin`] loads one `skill_dir`'s manifest into an
//!       `AgentPlugin`; [`install_discovered_skill_plugins`] walks every
//!       subdirectory of `<project_dir>/.trusty-agents/skills/` that
//!       contains a `manifest.json` and installs the union via
//!       `agent_plugin::install_plugins`. Both are best-effort: a missing
//!       skills directory, a malformed manifest, or an already-populated
//!       plugin registry are logged and skipped, never fatal — this must
//!       never block `tagent` from starting (safe-defaults: no skill wired
//!       is the pre-existing behaviour, not a crash).
//! Test: `manifest_parses_sample_skill_shape`, `build_plugin_reads_every_declared_tool`,
//!       `execute_dispatches_to_python_and_parses_json` (spawns a sample
//!       skill's `python3` command, written to a temp dir; skipped with a
//!       loud stderr note if `python3` is not on `PATH`, matching this
//!       workspace's fail-open convention for optional external tooling),
//!       `execute_maps_python_error_key_to_recoverable_result`,
//!       `install_discovered_skill_plugins_is_noop_on_missing_dir`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use trusty_agents_common::AgentPlugin;

use crate::tools::traits::{ToolExecutor, ToolResult};

/// Wall-clock budget for one skill subprocess call. Generous relative to
/// `run_bash`'s 30s because a cold `uv run` may need to resolve/sync a venv
/// on first invocation.
const SKILL_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(60);

/// On-disk `manifest.json` shape for one Python skill.
///
/// Why: The single source of truth a skill author edits; everything else in
///      this module derives from it.
/// What: `persona` is the exact persona name (must match the agent.toml's
///       `[agent].name`) this skill's tools are scoped to. `python.dir` is
///       resolved relative to the skill directory (the directory containing
///       `manifest.json`); `python.command` is the full argv template the
///       tool name gets appended to.
/// Test: `manifest_parses_sample_skill_shape`.
#[derive(Debug, Deserialize)]
struct SkillManifest {
    persona: String,
    python: PythonSpec,
    tools: Vec<ManifestTool>,
}

#[derive(Debug, Deserialize)]
struct PythonSpec {
    dir: String,
    command: Vec<String>,
}

/// One declared tool: name, LLM-facing description, and its JSON-schema
/// `parameters` object.
#[derive(Debug, Clone, Deserialize)]
struct ManifestTool {
    name: String,
    description: String,
    input_schema: Value,
}

/// Loads and parses `<skill_dir>/manifest.json`.
///
/// Why: Isolated so `build_plugin` and tests can share one error-context
///      story ("which file, which skill dir") instead of duplicating it.
/// What: Reads the file, parses as `SkillManifest`. Errors are always
///       `anyhow` with the offending path attached.
/// Test: `manifest_parses_sample_skill_shape`, `load_manifest_missing_file_errors`.
fn load_manifest(skill_dir: &Path) -> Result<SkillManifest> {
    let manifest_path = skill_dir.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parsing {} as a skill manifest", manifest_path.display()))
}

/// A single Python-skill-backed tool, generic over which skill it came from.
///
/// Why: One executor shape regardless of skill — the tool name, its schema,
///      the working directory, and the command argv are all data, not code.
/// What: `execute()` spawns `command` (working dir `working_dir`) with
///       `tool.name` appended, feeds `args` (serialised) on stdin, and
///       translates stdout into a `ToolResult`.
/// Test: `execute_dispatches_to_python_and_parses_json`,
///       `execute_maps_python_error_key_to_recoverable_result`,
///       `execute_reports_spawn_failure_as_recoverable`.
pub struct PythonSkillToolExecutor {
    tool: ManifestTool,
    working_dir: PathBuf,
    command: Vec<String>,
}

#[async_trait]
impl ToolExecutor for PythonSkillToolExecutor {
    fn name(&self) -> &str {
        &self.tool.name
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.tool.name,
                "description": self.tool.description,
                "parameters": self.tool.input_schema,
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some((program, rest)) = self.command.split_first() else {
            return ToolResult::fatal(format!(
                "python skill '{}': manifest's python.command is empty",
                self.tool.name
            ));
        };

        let mut cmd = Command::new(program);
        cmd.args(rest)
            .arg(&self.tool.name)
            .current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::err(format!(
                    "python skill '{}': failed to spawn '{program}': {e}",
                    self.tool.name
                ));
            }
        };

        if let Some(mut stdin) = child.stdin.take() {
            let payload = args.to_string();
            if let Err(e) = stdin.write_all(payload.as_bytes()).await {
                return ToolResult::err(format!(
                    "python skill '{}': failed to write stdin: {e}",
                    self.tool.name
                ));
            }
            // Drop closes the pipe so the child sees EOF on stdin.
            drop(stdin);
        }

        let output =
            match tokio::time::timeout(SKILL_SUBPROCESS_TIMEOUT, child.wait_with_output()).await {
                Err(_) => {
                    return ToolResult::err(format!(
                        "python skill '{}' timed out after {}s",
                        self.tool.name,
                        SKILL_SUBPROCESS_TIMEOUT.as_secs()
                    ));
                }
                Ok(Err(e)) => {
                    return ToolResult::err(format!(
                        "python skill '{}': failed while waiting for exit: {e}",
                        self.tool.name
                    ));
                }
                Ok(Ok(o)) => o,
            };

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return ToolResult::err(format!(
                "python skill '{}' produced no stdout (exit {:?}); stderr: {}",
                self.tool.name,
                output.status.code(),
                truncate(&stderr, 2000)
            ));
        }

        match serde_json::from_str::<Value>(&stdout) {
            Ok(v) => {
                if let Some(msg) = v.get("error").and_then(Value::as_str) {
                    ToolResult::err(msg.to_string())
                } else {
                    ToolResult::ok(v.to_string())
                }
            }
            Err(e) => ToolResult::err(format!(
                "python skill '{}': stdout was not valid JSON ({e}): {}",
                self.tool.name,
                truncate(&stdout, 2000)
            )),
        }
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}... [truncated]")
    }
}

/// Builds the `AgentPlugin` for one skill directory.
///
/// Why: Shared by `install_discovered_skill_plugins` and tests, so both
///      exercise identical manifest → executor construction.
/// What: Loads `<skill_dir>/manifest.json`, resolves `python.dir` relative
///       to `skill_dir`, and wraps each declared tool in a
///       `PythonSkillToolExecutor`.
/// Test: `build_plugin_reads_every_declared_tool`.
pub fn build_plugin(skill_dir: &Path) -> Result<AgentPlugin> {
    let manifest = load_manifest(skill_dir)?;
    if manifest.tools.is_empty() {
        bail!(
            "skill manifest at {} declares zero tools",
            skill_dir.display()
        );
    }
    let working_dir = skill_dir.join(&manifest.python.dir);
    let tools: Vec<std::sync::Arc<dyn ToolExecutor>> = manifest
        .tools
        .into_iter()
        .map(|tool| {
            std::sync::Arc::new(PythonSkillToolExecutor {
                tool,
                working_dir: working_dir.clone(),
                command: manifest.python.command.clone(),
            }) as std::sync::Arc<dyn ToolExecutor>
        })
        .collect();
    Ok(AgentPlugin::new(manifest.persona, tools))
}

/// Discovers every Python skill under `<project_dir>/.trusty-agents/skills/`
/// and installs them all as agent plugins in one `install_plugins` call.
///
/// Why: Called once at startup (see `runtime::startup`). Best-effort by
///      design — a skills directory that doesn't exist yet (most projects),
///      a skill with a malformed manifest, or a plugin registry some other
///      caller already populated must never stop `tagent` from starting.
/// What: Scans immediate subdirectories of the skills dir for a
///       `manifest.json`, builds a plugin per hit, and installs the whole
///       batch. Failures are logged at `warn` and skipped per-skill; if
///       `install_plugins` itself fails (already called), that's logged too.
/// Test: `install_discovered_skill_plugins_is_noop_on_missing_dir` (the only
///       part of this function safe to unit-test without racing the
///       process-global `OnceLock` other tests in this binary also touch —
///       see `agent_plugin.rs`'s own best-effort test for why).
pub fn install_discovered_skill_plugins(project_dir: &Path) {
    let skills_root = project_dir.join(".trusty-agents").join("skills");
    let entries = match std::fs::read_dir(&skills_root) {
        Ok(e) => e,
        Err(_) => return, // no skills dir yet — nothing to do, not an error
    };

    let mut plugins = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join("manifest.json").is_file() {
            continue;
        }
        match build_plugin(&path) {
            Ok(plugin) => plugins.push(plugin),
            Err(e) => {
                tracing::warn!(
                    skill_dir = %path.display(),
                    error = %e,
                    "failed to load python skill manifest; skipping"
                );
            }
        }
    }

    if plugins.is_empty() {
        return;
    }

    let count = plugins.len();
    if let Err(rejected) = crate::tools::agent_plugin::install_plugins(plugins) {
        tracing::warn!(
            count = rejected.len(),
            "install_plugins already called; discovered python skill plugins dropped"
        );
    } else {
        tracing::info!(count, "installed python skill plugin(s) discovered on disk");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The Python script the sample skill runs: echoes its arguments for
    /// `echo_args` and returns a top-level `"error"` for any other tool.
    const SAMPLE_SCRIPT: &str = r#"import json, sys
tool = sys.argv[-1]
args = json.load(sys.stdin)
if tool == "echo_args":
    print(json.dumps({"tool": tool, "echo": args}))
else:
    print(json.dumps({"error": "unknown tool " + tool}))
"#;

    /// Writes a neutral two-tool Python skill into a temp dir, so the bridge
    /// is tested against a real manifest and subprocess without depending on
    /// any bundled skill.
    fn sample_skill_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let py = tmp.path().join("python");
        std::fs::create_dir_all(&py).unwrap();
        std::fs::write(py.join("skill.py"), SAMPLE_SCRIPT).unwrap();
        let manifest = json!({
            "persona": "example-assistant",
            "python": {"dir": "python", "command": ["python3", "skill.py"]},
            "tools": [
                {"name": "echo_args", "description": "Echo the arguments.",
                 "input_schema": {"type": "object", "properties": {}}},
                {"name": "always_error", "description": "Always fails.",
                 "input_schema": {"type": "object", "properties": {}}}
            ]
        });
        std::fs::write(tmp.path().join("manifest.json"), manifest.to_string()).unwrap();
        tmp
    }

    fn python3_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn manifest_parses_sample_skill_shape() {
        let dir = sample_skill_dir();
        let manifest = load_manifest(dir.path()).expect("manifest should parse");
        assert_eq!(manifest.persona, "example-assistant");
        assert_eq!(manifest.python.dir, "python");
        let names: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["echo_args", "always_error"]);
    }

    #[test]
    fn load_manifest_missing_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_manifest(tmp.path()).is_err());
    }

    #[test]
    fn build_plugin_reads_every_declared_tool() {
        let dir = sample_skill_dir();
        let plugin = build_plugin(dir.path()).expect("plugin should build");
        assert_eq!(plugin.persona_name, "example-assistant");
        assert_eq!(plugin.tools.len(), 2);
        // Every tool's schema must be a well-formed OpenAI function schema.
        for tool in &plugin.tools {
            let schema = tool.schema();
            assert_eq!(schema["type"], "function");
            assert_eq!(schema["function"]["name"], tool.name());
        }
    }

    #[test]
    fn build_plugin_rejects_manifest_with_zero_tools() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("manifest.json"),
            r#"{"persona": "x", "python": {"dir": "python", "command": ["python3"]}, "tools": []}"#,
        )
        .unwrap();
        assert!(build_plugin(tmp.path()).is_err());
    }

    #[tokio::test]
    async fn execute_dispatches_to_python_and_parses_json() {
        if !python3_available() {
            eprintln!("SKIP: `python3` not on PATH — cannot exercise the skill subprocess");
            return;
        }
        let dir = sample_skill_dir();
        let plugin = build_plugin(dir.path()).expect("plugin should build");
        let tool = plugin
            .tools
            .iter()
            .find(|t| t.name() == "echo_args")
            .expect("echo_args present");

        let result = tool.execute(json!({ "filter_by": "team" })).await;
        assert!(
            !result.is_error(),
            "expected success from the sample skill, got: {}",
            result.content()
        );
        let parsed: Value = serde_json::from_str(result.content()).expect("content must be JSON");
        assert_eq!(parsed["tool"], "echo_args");
        assert_eq!(parsed["echo"]["filter_by"], "team");
    }

    #[tokio::test]
    async fn execute_maps_python_error_key_to_recoverable_result() {
        if !python3_available() {
            eprintln!("SKIP: `python3` not on PATH — cannot exercise the skill subprocess");
            return;
        }
        let dir = sample_skill_dir();
        let plugin = build_plugin(dir.path()).expect("plugin should build");
        let tool = plugin
            .tools
            .iter()
            .find(|t| t.name() == "always_error")
            .expect("always_error present");

        let result = tool.execute(json!({})).await;
        assert!(
            result.is_error(),
            "a top-level error key should surface as an error"
        );
        assert!(
            !result.is_fatal(),
            "python skill errors must be recoverable"
        );
    }

    #[tokio::test]
    async fn execute_reports_spawn_failure_as_recoverable() {
        let tool = PythonSkillToolExecutor {
            tool: ManifestTool {
                name: "does_not_matter".into(),
                description: "".into(),
                input_schema: json!({}),
            },
            working_dir: PathBuf::from("/"),
            command: vec!["definitely-not-a-real-binary-xyz".into()],
        };
        let result = tool.execute(json!({})).await;
        assert!(result.is_error());
        assert!(!result.is_fatal());
    }

    #[test]
    fn install_discovered_skill_plugins_is_noop_on_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Must not panic even though `<tmp>/.trusty-agents/skills` doesn't exist.
        install_discovered_skill_plugins(tmp.path());
    }
}
