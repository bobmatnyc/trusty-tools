//! Generic bridge: turns a Python skill directory's `manifest.json` into
//! registered `ToolExecutor`s, with zero per-skill Rust code.
//!
//! Why: cto-assistant's `agent.toml` declares four `[tools].allow` entries
//!      (`query_headcount`, `query_budget`, `query_risks`,
//!      `query_work_classification`) that nothing in the live dispatch path
//!      implements (#3700) — the Rust-native implementation
//!      (the former `crates/cto-assistant`, dissolved in #3732, over
//!      `crates/tc-services::cto_db` + `crates/trusty-cto-db`) compiled and
//!      was fully tested, but its only `install_plugins(...)` call site was
//!      removed by PR #3310, and resurrecting it would re-embed
//!      CTO-specific business logic in Rust —
//!      exactly what #3656 objects to (DOC-41 §2.0 "declarative-only"
//!      agents). The owner's directive is to move that business logic into a
//!      *skill* instead (Python code + data, bundled together), while
//!      keeping trusty-agents itself free of CTO-specific code. This module
//!      is the generic seam that makes that possible: it knows nothing about
//!      "cto-db" specifically, only how to read a skill manifest and run it.
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
//! Test: `manifest_parses_cto_db_shape`, `build_plugin_reads_four_tools`,
//!       `execute_dispatches_to_python_and_parses_json` (spawns the real
//!       `cto-db` skill's `uv run` command against its bundled fixture DB,
//!       opted into via `CtoDbEnvGuard` since #4860; skipped with a loud
//!       stderr note if `uv` is not on `PATH`, matching this workspace's
//!       fail-open convention for optional external tooling),
//!       `execute_without_a_configured_db_surfaces_the_refusal`,
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
/// Test: `manifest_parses_cto_db_shape`.
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
/// Test: `manifest_parses_cto_db_shape`, `load_manifest_missing_file_errors`.
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
/// Test: `build_plugin_reads_four_tools`.
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

    fn cto_db_skill_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".trusty-agents/skills/cto-db")
    }

    fn uv_available() -> bool {
        std::process::Command::new("uv")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Hold `$HOME` still for the duration of a test that shells out to `uv`
    /// (#4414).
    ///
    /// Why: `uv` resolves its cache from `$HOME` (`$HOME/.cache/uv`) at spawn
    /// time, and 161 statements across this crate's tests reassign `HOME`
    /// process-wide to a `TempDir`. Under parallel load the sequence that
    /// actually bit was: a sibling test set `HOME=<tempdir>`, `uv` derived its
    /// cache path from that tempdir, and the sibling's `TempDir` was then
    /// dropped — deleting the tree out from under the running interpreter.
    /// Captured failures name the vanished path directly:
    /// `Caused by: No such file or directory (os error 2) at path
    /// ".../T/.tmpHR50aj/.cache/uv/.tmptfs807"`, and
    /// `ModuleNotFoundError: No module named 'python'` for the same reason.
    /// Measured on `main`: 3 failures in 5 full-suite runs.
    ///
    /// What: acquires `test_env::HOME_LOCK`, the mutex this crate's HOME
    /// convention is built on. Verified exhaustive for this crate before relying
    /// on it: all 161 HOME-mutating statements — 116 `set_var("HOME", ..)` plus
    /// 45 `remove_var("HOME")`, across 72 test fns — sit inside a function that
    /// holds `HOME_LOCK`, so holding it here excludes every writer. Nothing is
    /// mutating `HOME`, and no sibling's `TempDir` is being reaped, while the
    /// `uv` subprocess runs. That makes the test hermetic with respect to the
    /// variable rather than merely likely to win the race; widening a timeout
    /// or retrying would have left the same race with a longer fuse.
    ///
    /// `unwrap_or_else(into_inner)` so one panicking test cannot poison the
    /// lock for its siblings — the same acquisition idiom the other 43 callers
    /// use. Plain `HOME_LOCK` rather than `test_env::lock_home()` because these
    /// tests exercise no production path guarded by
    /// `home_lock_held_by_this_thread` (only `listeners::store::events_dir` is).
    /// Test: [`execute_dispatches_to_python_and_parses_json`],
    /// [`execute_maps_python_error_key_to_recoverable_result`], and
    /// [`execute_without_a_configured_db_surfaces_the_refusal`] — the tests
    /// that spawn `uv`. [`execute_reports_spawn_failure_as_recoverable`]
    /// deliberately does not: it spawns a nonexistent binary and never reads
    /// `HOME`.
    const CTO_DB_PATH_ENV: &str = "CTO_DB_PATH";
    const CTO_DB_USE_FIXTURE_ENV: &str = "CTO_DB_USE_FIXTURE";

    /// Pin the cto-db skill's data source for one test body (#4860).
    ///
    /// Why: the skill refuses to answer unless a database is configured, so a
    /// test wanting the bundled fixture must opt in by name, and a test proving
    /// the refusal must ensure neither variable is set. A developer's real
    /// `CTO_DB_PATH` is dropped either way so the suite never queries it.
    /// What: RAII over both variables, restoring their prior values on drop.
    /// Every caller also holds [`hold_home_for_uv`]'s `HOME_LOCK`, the mutex
    /// this crate's env-mutating tests serialize on.
    /// Test: used by [`execute_dispatches_to_python_and_parses_json`],
    /// [`execute_maps_python_error_key_to_recoverable_result`], and
    /// [`execute_without_a_configured_db_surfaces_the_refusal`].
    struct CtoDbEnvGuard {
        prev_path: Option<String>,
        prev_fixture: Option<String>,
    }

    impl CtoDbEnvGuard {
        fn new(use_fixture: bool) -> Self {
            let guard = Self {
                prev_path: std::env::var(CTO_DB_PATH_ENV).ok(),
                prev_fixture: std::env::var(CTO_DB_USE_FIXTURE_ENV).ok(),
            };
            // SAFETY: serialized by the caller's `HOME_LOCK` guard, the same
            // convention this module's `$HOME` mutation already relies on.
            unsafe {
                std::env::remove_var(CTO_DB_PATH_ENV);
                if use_fixture {
                    std::env::set_var(CTO_DB_USE_FIXTURE_ENV, "1");
                } else {
                    std::env::remove_var(CTO_DB_USE_FIXTURE_ENV);
                }
            }
            guard
        }

        fn fixture() -> Self {
            Self::new(true)
        }

        fn unconfigured() -> Self {
            Self::new(false)
        }
    }

    impl Drop for CtoDbEnvGuard {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe {
                for (key, prev) in [
                    (CTO_DB_PATH_ENV, self.prev_path.take()),
                    (CTO_DB_USE_FIXTURE_ENV, self.prev_fixture.take()),
                ] {
                    match prev {
                        Some(v) => std::env::set_var(key, v),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    fn hold_home_for_uv() -> std::sync::MutexGuard<'static, ()> {
        crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn manifest_parses_cto_db_shape() {
        let manifest = load_manifest(&cto_db_skill_dir()).expect("manifest should parse");
        assert_eq!(manifest.persona, "cto-assistant");
        assert_eq!(manifest.python.dir, "python");
        assert_eq!(manifest.tools.len(), 4);
        let names: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();
        for expected in [
            "query_headcount",
            "query_budget",
            "query_risks",
            "query_work_classification",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
    }

    #[test]
    fn load_manifest_missing_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_manifest(tmp.path()).is_err());
    }

    #[test]
    fn build_plugin_reads_four_tools() {
        let plugin = build_plugin(&cto_db_skill_dir()).expect("plugin should build");
        assert_eq!(plugin.persona_name, "cto-assistant");
        assert_eq!(plugin.tools.len(), 4);
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

    // #4414: holds the guard across `.await` deliberately — the whole point is
    // that `$HOME` must not move while the `uv` subprocess is alive. The default
    // `#[tokio::test]` runtime is current-thread, which pins this test (every
    // `.await` included) to one OS thread, so the guard is sound here.
    #[allow(
        clippy::await_holding_lock,
        reason = "#4414: holding HOME_LOCK across the uv subprocess IS the fix"
    )]
    #[tokio::test]
    async fn execute_dispatches_to_python_and_parses_json() {
        if !uv_available() {
            eprintln!("SKIP: `uv` not on PATH — cannot exercise the real cto-db skill subprocess");
            return;
        }
        // #4414: pin $HOME for the whole body — see `hold_home_for_uv`.
        let _home = hold_home_for_uv();
        // #4860: the fixture is opt-in now, so ask for it by name.
        let _db = CtoDbEnvGuard::fixture();
        let plugin = build_plugin(&cto_db_skill_dir()).expect("plugin should build");
        let tool = plugin
            .tools
            .iter()
            .find(|t| t.name() == "query_headcount")
            .expect("query_headcount present");

        let result = tool.execute(json!({ "filter_by": "team" })).await;
        assert!(
            !result.is_error(),
            "expected success against the bundled fixture db, got: {}",
            result.content()
        );
        let parsed: Value = serde_json::from_str(result.content()).expect("content must be JSON");
        assert_eq!(parsed["filter_by"], "team");
        assert!(parsed["groups"].as_array().is_some_and(|g| !g.is_empty()));
        // #4860: fixture output must be labelled, not indistinguishable from a
        // real answer.
        assert_eq!(parsed["is_fixture"], true);
        assert!(
            parsed["db_path"]
                .as_str()
                .is_some_and(|p| p.ends_with("cto_fixture.db"))
        );
    }

    /// #4860: with no database configured the bridge must surface a refusal,
    /// not five invented names presented as a real headcount.
    #[allow(
        clippy::await_holding_lock,
        reason = "#4414: holding HOME_LOCK across the uv subprocess IS the fix"
    )]
    #[tokio::test]
    async fn execute_without_a_configured_db_surfaces_the_refusal() {
        if !uv_available() {
            eprintln!("SKIP: `uv` not on PATH — cannot exercise the real cto-db skill subprocess");
            return;
        }
        let _home = hold_home_for_uv();
        let _db = CtoDbEnvGuard::unconfigured();
        let plugin = build_plugin(&cto_db_skill_dir()).expect("plugin should build");
        let tool = plugin
            .tools
            .iter()
            .find(|t| t.name() == "query_headcount")
            .expect("query_headcount present");

        let result = tool.execute(json!({ "filter_by": "team" })).await;
        assert!(result.is_error(), "an unconfigured skill must not answer");
        assert!(
            !result.is_fatal(),
            "python skill errors must be recoverable"
        );
        assert!(
            result.content().contains("CTO_DB_PATH"),
            "the refusal must name the variable that fixes it, got: {}",
            result.content()
        );
        assert!(
            !result.content().contains("groups"),
            "no fixture rows may leak into the refusal, got: {}",
            result.content()
        );
    }

    // #4414: holds the guard across `.await` deliberately — the whole point is
    // that `$HOME` must not move while the `uv` subprocess is alive. The default
    // `#[tokio::test]` runtime is current-thread, which pins this test (every
    // `.await` included) to one OS thread, so the guard is sound here.
    #[allow(
        clippy::await_holding_lock,
        reason = "#4414: holding HOME_LOCK across the uv subprocess IS the fix"
    )]
    #[tokio::test]
    async fn execute_maps_python_error_key_to_recoverable_result() {
        if !uv_available() {
            eprintln!("SKIP: `uv` not on PATH — cannot exercise the real cto-db skill subprocess");
            return;
        }
        // #4414: pin $HOME for the whole body — see `hold_home_for_uv`.
        let _home = hold_home_for_uv();
        // #4860: opt into the fixture so this reaches the unknown-filter path
        // rather than stopping at the unconfigured-database refusal.
        let _db = CtoDbEnvGuard::fixture();
        let plugin = build_plugin(&cto_db_skill_dir()).expect("plugin should build");
        let tool = plugin
            .tools
            .iter()
            .find(|t| t.name() == "query_headcount")
            .expect("query_headcount present");

        // "bogus" is not one of query_headcount's documented filter_by values.
        let result = tool.execute(json!({ "filter_by": "bogus" })).await;
        assert!(
            result.is_error(),
            "unknown filter_by should surface as an error"
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
