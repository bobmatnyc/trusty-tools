//! `meta` command handler — standalone metaharness (#1045, WI-1..WI-4).
//!
//! Why: issue #1045 builds an M1 POC metaharness that boots without the
//! trusty-mpm daemon or the `claude` CLI and drives PM → sub-agent delegation
//! in-process via trusty-code (Seam A). WI-1 stood up the `meta run` entry point;
//! WI-2 wired a trusty-code [`ToolRegistry`](trusty_code::tools::ToolRegistry);
//! WI-4 (this change, closing #1030) replaces the WI-2 `NoopAgentRunner` with the
//! live [`InProcessAgentRunner`](trusty_code::runner::InProcessAgentRunner): a
//! bare `meta run` still prints the registry summary (offline, no LLM), while
//! `meta run --demo` performs a *real* PM → engineer delegation against a live
//! OpenRouter model and persists a combined transcript.
//! What: [`meta`] dispatches `MetaAction`; [`run`] validates `--project`, and for
//! `--demo` materialises the bundled PM + engineer configs, constructs the live
//! [`Orchestrator`](orchestrator::Orchestrator) over a shared LLM client, runs one
//! delegation cycle, writes the transcript under `.trusty-mpm/meta-runs/`, and
//! prints a summary; without `--demo` it falls back to the WI-2 registry summary.
//! Pure helpers ([`resolve_project`], [`wi2_summary`], [`demo_task`]) carry the
//! testable logic; agent configs, the transcript schema, and the orchestrator
//! live in sibling submodules.
//! Test: `meta_*` unit tests in this module's `tests` block; submodule tests in
//! `agents`/`transcript`/`orchestrator`; CLI parsing in `tests.rs`.

mod agents;
mod orchestrator;
mod registry;
mod transcript;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use tracing::{error, info};
use trusty_code::llm::{LlmClient, LlmClientConfig};
use trusty_code::runner::InProcessRunnerConfig;

use self::orchestrator::Orchestrator;
use self::registry::{build_meta_registry, registry_tool_names};
use crate::cli::MetaAction;

/// Status string stamped into the WI-2 registry-only run summary.
///
/// Why: the summary `status` is a magic string consumed by tests (and tooling
/// that scrapes `meta run` output); centralising it keeps the producer and every
/// assertion in lockstep.
/// What: the literal `"wi2"` — signalling a registry-assembly-only run (no demo).
/// Test: `meta_wi2_summary_reports_status` asserts the emitted value.
pub(crate) const STATUS_WI2: &str = "wi2";

/// Subdirectory (under the project) where transcripts and agent configs live.
///
/// Why: The demo persists its combined transcript and materialises the bundled
/// agent configs under a predictable, project-scoped location so operators can
/// inspect a run after the fact (#1045 success criterion).
/// What: the literal `".trusty-mpm"` directory name.
/// Test: `meta_run_dir_paths_are_project_scoped`.
pub(crate) const META_STATE_DIR: &str = ".trusty-mpm";

/// The bundled demo task the PM is asked to accomplish.
///
/// Why: `--demo` runs a fixed, checkable task (write a known file) so a run's
/// success is verifiable without operator input (#1045: writes
/// `hello_metaharness.txt`, verifies content, exits 0).
/// What: instructs the PM to have the engineer create `hello_metaharness.txt`
/// with a known line of content.
/// Test: `meta_demo_task_names_expected_file`.
pub(crate) const DEMO_ARTIFACT: &str = "hello_metaharness.txt";

/// `meta` subcommand dispatcher — route a parsed [`MetaAction`] to its handler.
///
/// Why: mirrors the other `tm` command groups by keeping `main`'s match arm thin
/// and folding verb dispatch into the module that owns the verbs.
/// What: matches the `MetaAction` and forwards `meta run` to [`run`] (async, so
/// the demo can drive the live agent loop without a nested runtime).
/// Test: covered by the handler unit tests via the `Run` arm; CLI parse
/// round-trips live in `tests.rs`.
pub(crate) async fn meta(action: MetaAction) -> anyhow::Result<()> {
    match action {
        MetaAction::Run { demo, project } => run(demo, project).await,
    }
}

/// The bundled demo task string handed to the PM.
///
/// Why: Isolating the task text keeps it unit-testable and the `run` handler
/// free of inline prose.
/// What: returns a one-line instruction to create [`DEMO_ARTIFACT`] with a known
/// body.
/// Test: `meta_demo_task_names_expected_file`.
pub(crate) fn demo_task() -> String {
    format!(
        "Create a file named `{DEMO_ARTIFACT}` in the project root containing exactly the line \
         `hello from the metaharness`. Delegate the file creation to the python-engineer agent."
    )
}

/// Execute one `meta run` invocation.
///
/// Why: this is the harness's primary entry point. Without `--demo` it stays an
/// offline registry-assembly smoke test (no LLM, no network). With `--demo` it is
/// the WI-4 deliverable (#1030): a real PM → engineer delegation driven by a live
/// OpenRouter model, with a combined transcript persisted for inspection.
/// What: initialises stderr tracing; resolves and validates `--project`; for the
/// non-demo path builds the registry and prints the [`wi2_summary`]; for the demo
/// path delegates to [`run_demo`].
/// Test: `meta_run_registry_summary_for_existing_project`,
/// `meta_run_errors_on_missing_project` exercise the offline paths; the live demo
/// is covered by the `orchestrator` tests (mock LLM) and an ignored live test.
pub(crate) async fn run(demo: bool, project: Option<PathBuf>) -> anyhow::Result<()> {
    init_meta_tracing();
    let project = resolve_project(project)?;

    if demo {
        return run_demo(&project).await;
    }

    let registry = build_meta_registry(&project);
    let tools = registry_tool_names(&registry);
    info!(
        demo,
        project = %project.display(),
        tools = ?tools,
        "meta run: tool registry assembled (offline summary — pass --demo to run the live harness)"
    );
    let summary = wi2_summary(demo, &project, &tools);
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

/// Run the live PM → engineer demo delegation and persist the transcript.
///
/// Why: This is the WI-4 / #1030 payoff — proof the in-process orchestrator
/// drives a real agent end-to-end. It requires a live LLM key; absent one it
/// fails with a clear, actionable error rather than silently mocking.
/// What: requires `OPENROUTER_API_KEY` (clear error if missing); materialises the
/// bundled PM + engineer configs under `<project>/.trusty-mpm/meta-agents/`; runs
/// the [`Orchestrator`] over a shared `LlmClient`; writes the combined transcript
/// under `<project>/.trusty-mpm/meta-runs/`; prints a JSON summary to stdout and
/// logs progress to stderr.
/// Test: side-effect/IO-heavy; the orchestration logic is covered offline by
/// `orchestrator::tests` (scripted LLM) and an ignored live end-to-end test.
async fn run_demo(project: &Path) -> anyhow::Result<()> {
    let config = LlmClientConfig::from_env().context(
        "meta run --demo requires a live LLM: set OPENROUTER_API_KEY in the environment. \
         (The wiring is fully covered offline by `cargo test -p trusty-mpm meta::orchestrator`.)",
    )?;
    let client = LlmClient::from_config(config).context("failed to build OpenRouter client")?;
    let llm = std::sync::Arc::new(client);

    let agents_dir = meta_agents_dir(project);
    agents::write_agent_configs(&agents_dir)
        .context("failed to materialise bundled agent configs")?;

    let task = demo_task();
    info!(
        project = %project.display(),
        agents_dir = %agents_dir.display(),
        "meta run --demo: driving live PM → engineer delegation"
    );

    let mut orchestrator = Orchestrator::new(llm, agents_dir, project.to_path_buf()).with_config(
        InProcessRunnerConfig {
            max_turns: 6,
            timeout_secs: 180,
        },
    );
    // Thread the project's CLAUDE.md (if any) into every assembled prompt so the
    // PM and engineer see the same project rules (parity-spec).
    if let Some(ctx) = read_project_context(project) {
        orchestrator = orchestrator.with_project_context(ctx);
    }

    let transcript = match orchestrator.run(&task).await {
        Ok(t) => t,
        Err(e) => {
            error!(error = %e, "meta run --demo: orchestration failed");
            return Err(e);
        }
    };

    let transcript_path = write_transcript(project, &transcript)?;
    info!(
        delegations = transcript.delegations.len(),
        artifacts = transcript.artifacts.len(),
        total_tokens = transcript.usage.total_tokens,
        transcript = %transcript_path.display(),
        "meta run --demo: delegation cycle complete"
    );

    let summary = json!({
        "status": "demo",
        "project": project.display().to_string(),
        "model": transcript.model,
        "delegations": transcript.delegations.len(),
        "artifacts": transcript.artifacts.iter().map(|a| &a.path).collect::<Vec<_>>(),
        "total_tokens": transcript.usage.total_tokens,
        "transcript": transcript_path.display().to_string(),
    });
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

/// Read the project's `CLAUDE.md`, if present, for prompt injection.
///
/// Why: The parity-spec wants the PM and every sub-agent to see the same project
/// rules; the project `CLAUDE.md` is that surface. Reading it here keeps the
/// orchestrator agnostic to where context comes from.
/// What: returns `Some(contents)` if `<project>/CLAUDE.md` reads successfully,
/// else `None` (a missing/unreadable file is not an error — context is optional).
/// Test: `meta_read_project_context_reads_existing_file`.
pub(crate) fn read_project_context(project: &Path) -> Option<String> {
    std::fs::read_to_string(project.join("CLAUDE.md")).ok()
}

/// Directory holding the bundled agent configs for a run.
///
/// Why: The runner loads agents from on-disk TOML; placing them under the
/// project's state dir keeps the run self-contained and inspectable.
/// What: returns `<project>/.trusty-mpm/meta-agents`.
/// Test: `meta_run_dir_paths_are_project_scoped`.
pub(crate) fn meta_agents_dir(project: &Path) -> PathBuf {
    project.join(META_STATE_DIR).join("meta-agents")
}

/// Directory where run transcripts are persisted.
///
/// Why: Operators inspect past runs; a stable location makes them discoverable
/// (#1045 success criterion).
/// What: returns `<project>/.trusty-mpm/meta-runs`.
/// Test: `meta_run_dir_paths_are_project_scoped`.
pub(crate) fn meta_runs_dir(project: &Path) -> PathBuf {
    project.join(META_STATE_DIR).join("meta-runs")
}

/// Compute the transcript filename for a run at instant `now`.
///
/// Why: Second-granularity filenames collide when two runs start within the same
/// wall-clock second, silently overwriting the earlier transcript. Deriving the
/// name from milliseconds-since-epoch makes intra-second collisions vanishingly
/// unlikely while keeping the name sortable and human-readable. Factoring this out
/// of [`write_transcript`] keeps the (otherwise IO-bound) naming logic unit-testable.
/// What: returns `run-<unix_ts_millis>.json`, falling back to `0` if the clock is
/// before the Unix epoch (which cannot happen for `SystemTime::now`).
/// Test: `meta_run_filename_uses_millisecond_precision`.
fn run_transcript_filename(now: std::time::SystemTime) -> String {
    let millis = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("run-{millis}.json")
}

/// Persist a combined transcript as pretty JSON, returning its path.
///
/// Why: The transcript is the run's auditable record; writing it to disk lets
/// downstream tooling (and humans) inspect the PM + engineer turns after exit.
/// What: creates the runs dir, writes `run-<unix_ts_millis>.json` with the
/// serialised transcript, and returns the path; surfaces an `anyhow` error on IO
/// failure. Millisecond (not second) precision is used for the filename so two
/// runs started within the same wall-clock second do not collide and overwrite
/// each other's transcript.
/// Test: `meta_run_filename_uses_millisecond_precision`; the transcript shape is
/// covered by `transcript::tests`.
fn write_transcript(
    project: &Path,
    transcript: &transcript::MetaTranscript,
) -> anyhow::Result<PathBuf> {
    let dir = meta_runs_dir(project);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create runs dir: {}", dir.display()))?;
    let path = dir.join(run_transcript_filename(std::time::SystemTime::now()));
    let body = serde_json::to_string_pretty(transcript)
        .context("failed to serialise transcript to JSON")?;
    std::fs::write(&path, body)
        .with_context(|| format!("failed to write transcript: {}", path.display()))?;
    Ok(path)
}

/// Resolve the optional `--project` argument to an existing absolute path.
///
/// Why: every later step operates relative to a concrete working directory, so
/// the bootstrap must fail fast and clearly if pointed at a missing path.
/// What: defaults a missing argument to the process cwd, canonicalises the path
/// (asserting existence) and returns the absolute form; returns an `anyhow` error
/// naming the offending path when it is absent or unreadable.
/// Test: `meta_resolve_project_accepts_existing_dir`,
/// `meta_resolve_project_rejects_missing_path`,
/// `meta_resolve_project_defaults_to_cwd`.
pub(crate) fn resolve_project(project: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let raw = match project {
        Some(p) => p,
        None => std::env::current_dir().context("failed to resolve current directory")?,
    };
    let resolved = std::fs::canonicalize(&raw)
        .with_context(|| format!("project path does not exist: {}", raw.display()))?;
    Ok(resolved)
}

/// Build the registry-only run summary as a JSON object (non-demo path).
///
/// Why: a bare `meta run` emits a single machine-readable line summarising the
/// assembled tool registry so downstream tooling can confirm the harness offered
/// the expected capabilities, without invoking an LLM.
/// What: returns
/// `{"status":"wi2","demo":<bool>,"project":"<abs path>","tools":[<names>]}`.
/// Test: `meta_wi2_summary_reports_status`,
/// `meta_wi2_summary_carries_demo_project_and_tools`.
pub(crate) fn wi2_summary(demo: bool, project: &Path, tools: &[String]) -> serde_json::Value {
    json!({
        "status": STATUS_WI2,
        "demo": demo,
        "project": project.display().to_string(),
        "tools": tools,
    })
}

/// Initialise stderr tracing for the short-lived `meta run` invocation.
///
/// Why: short-lived `tm` subcommands skip the daemon's subscriber init, but the
/// metaharness requires structured logging that honours `RUST_LOG`. `try_init`
/// keeps this idempotent — it silently no-ops if a subscriber already exists.
/// What: installs a stderr `fmt` subscriber filtered by `RUST_LOG` (default
/// `info`); ignores the error when a subscriber already exists.
/// Test: side-effect-only; exercised indirectly by the handler tests.
fn init_meta_tracing() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_resolve_project_accepts_existing_dir() {
        let tmp = std::env::temp_dir();
        let resolved = resolve_project(Some(tmp.clone())).expect("temp dir resolves");
        assert!(resolved.is_absolute());
        assert!(resolved.exists());
    }

    #[test]
    fn meta_resolve_project_rejects_missing_path() {
        let missing = PathBuf::from("/nonexistent-meta-bootstrap-xyz-12345");
        let err = resolve_project(Some(missing)).expect_err("missing path must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("does not exist"),
            "error should name the missing path, got: {msg}"
        );
    }

    #[test]
    fn meta_resolve_project_defaults_to_cwd() {
        let cwd = std::env::current_dir().expect("cwd available");
        let resolved = resolve_project(None).expect("cwd resolves");
        assert_eq!(
            resolved,
            std::fs::canonicalize(&cwd).expect("cwd canonicalises")
        );
    }

    #[test]
    fn meta_wi2_summary_reports_status() {
        let tools = vec!["bash".to_string()];
        let summary = wi2_summary(true, Path::new("/tmp"), &tools);
        assert_eq!(summary["status"], STATUS_WI2);
    }

    #[test]
    fn meta_wi2_summary_carries_demo_project_and_tools() {
        let tools = vec!["bash".to_string(), "read_file".to_string()];
        let summary = wi2_summary(false, Path::new("/work/p"), &tools);
        assert_eq!(summary["demo"], false);
        assert_eq!(summary["project"], "/work/p");
        assert_eq!(summary["tools"], json!(["bash", "read_file"]));
    }

    #[test]
    fn meta_run_demo_emits_expected_tool_list() {
        // The registry-backed summary over an existing project must list every
        // metaharness tool — guards the run()→registry wiring.
        let tmp = std::env::temp_dir();
        let registry = build_meta_registry(&tmp);
        let tools = registry_tool_names(&registry);
        assert_eq!(
            tools,
            vec![
                "bash".to_string(),
                "delegate_to_agent".to_string(),
                "edit".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn meta_run_registry_summary_for_existing_project() {
        // The non-demo path over an existing project must exit Ok (exit 0) so the
        // offline scaffold stays smoke-testable without an LLM key.
        let tmp = std::env::temp_dir();
        run(false, Some(tmp))
            .await
            .expect("registry summary succeeds");
    }

    #[tokio::test]
    async fn meta_run_errors_on_missing_project() {
        let missing = PathBuf::from("/nonexistent-meta-bootstrap-run-xyz-98765");
        assert!(run(true, Some(missing)).await.is_err());
    }

    #[test]
    fn meta_demo_task_names_expected_file() {
        assert!(
            demo_task().contains(DEMO_ARTIFACT),
            "demo task must name the artifact file"
        );
        assert!(
            demo_task().contains("python-engineer"),
            "demo task must direct delegation to the engineer"
        );
    }

    #[test]
    fn meta_read_project_context_reads_existing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Absent file → None.
        assert!(read_project_context(tmp.path()).is_none());
        // Present file → Some(contents).
        std::fs::write(tmp.path().join("CLAUDE.md"), "PROJECT RULES").expect("write CLAUDE.md");
        assert_eq!(
            read_project_context(tmp.path()).as_deref(),
            Some("PROJECT RULES")
        );
    }

    #[test]
    fn meta_run_filename_uses_millisecond_precision() {
        // Two instants in the same wall-clock second but different milliseconds
        // must yield distinct filenames (fix #3: no same-second overwrite).
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_123);
        let same_second = base + std::time::Duration::from_millis(456);
        let a = run_transcript_filename(base);
        let b = run_transcript_filename(same_second);
        assert_eq!(a, "run-1700000000123.json");
        assert_eq!(b, "run-1700000000579.json");
        assert_ne!(
            a, b,
            "millisecond precision must disambiguate same-second runs"
        );
    }

    #[test]
    fn meta_run_dir_paths_are_project_scoped() {
        let project = Path::new("/work/proj");
        assert_eq!(
            meta_agents_dir(project),
            Path::new("/work/proj/.trusty-mpm/meta-agents")
        );
        assert_eq!(
            meta_runs_dir(project),
            Path::new("/work/proj/.trusty-mpm/meta-runs")
        );
    }
}
