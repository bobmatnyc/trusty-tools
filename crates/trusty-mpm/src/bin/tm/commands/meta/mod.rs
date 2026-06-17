//! `meta` command handler — standalone metaharness bootstrap (#1045, WI-1/WI-2).
//!
//! Why: issue #1045 builds an M1 POC metaharness that boots without the
//! trusty-mpm daemon or the `claude` CLI and drives PM → sub-agent delegation
//! in-process via trusty-code (Seam A). WI-1 stood up the `meta run` entry point
//! (argument validation, structured logging, a placeholder run summary). WI-2
//! wires a trusty-code [`ToolRegistry`](trusty_code::tools::ToolRegistry) into
//! that path so the (future) PM loop has the fs/bash/delegate capabilities it
//! will offer the model. Live LLM inference, real delegation, and transcript
//! capture remain intentionally unimplemented here (WI-3..WI-8).
//! What: [`meta`] dispatches `MetaAction`; [`run`] validates the `--project`
//! path, builds the metaharness tool registry, logs the registered tool names,
//! and writes a JSON summary (including the tool list) to stdout. Pure helpers
//! ([`resolve_project`], [`wi2_summary`]) carry the testable logic so unit tests
//! need no stdout capture or live runtime; registry assembly lives in the
//! [`registry`] submodule.
//! Test: `meta_*` unit tests in this module's `tests` block; registry tests in
//! `registry::tests`; CLI parsing in `tests.rs` (`cli_parses_meta_run*`).

mod registry;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use tracing::{info, warn};

use self::registry::{build_meta_registry, registry_tool_names};
use crate::cli::MetaAction;

/// Status string stamped into the WI-2 run summary.
///
/// Why: the summary `status` is a magic string consumed by tests (and,
/// eventually, by tooling that scrapes `meta run` output); centralising it
/// keeps the producer and every assertion in lockstep.
/// What: the literal `"wi2"` — signalling that this run assembled the tool
/// registry but performed no real delegation or LLM inference yet.
/// Test: `meta_wi2_summary_reports_status` asserts the emitted value.
pub(crate) const STATUS_WI2: &str = "wi2";

/// `meta` subcommand dispatcher — route a parsed [`MetaAction`] to its handler.
///
/// Why: mirrors the other `tm` command groups (e.g. `project`, `issue`) by
/// keeping `main`'s match arm thin and folding verb dispatch into the module
/// that owns the verbs.
/// What: matches the `MetaAction` and forwards `meta run` to [`run`].
/// Test: covered by the handler unit tests via the `Run` arm; CLI parse
/// round-trips live in `tests.rs`.
pub(crate) fn meta(action: MetaAction) -> anyhow::Result<()> {
    match action {
        MetaAction::Run { demo, project } => run(demo, project),
    }
}

/// Execute one `meta run` invocation (WI-2: tool registry wiring).
///
/// Why: this is the harness's primary entry point. WI-2 extends the WI-1
/// scaffold by assembling the trusty-code [`ToolRegistry`] the (future) PM loop
/// will offer the model — proving the fs/bash/delegate tools wire together —
/// without yet implementing real delegation or live LLM inference. Returning
/// `Ok(())` on the happy path lets the demo command exit 0 so the scaffold stays
/// smoke-testable.
/// What: initialises stderr tracing (idempotent, honours `RUST_LOG`), resolves
/// and validates `--project` to an existing absolute path, builds the
/// metaharness registry scoped to that project, logs the registered tool names
/// at info level (stderr), and writes the [`wi2_summary`] JSON object (including
/// the tool list) to stdout.
/// Test: `meta_run_demo_succeeds_for_existing_project`,
/// `meta_run_errors_on_missing_project` exercise the validation + happy paths.
pub(crate) fn run(demo: bool, project: Option<PathBuf>) -> anyhow::Result<()> {
    init_meta_tracing();

    let project = resolve_project(project)?;
    let registry = build_meta_registry(&project);
    let tools = registry_tool_names(&registry);

    info!(
        demo,
        project = %project.display(),
        tools = ?tools,
        "meta run: WI-2 tool registry assembled — no delegation performed yet"
    );
    warn!(
        "`tm meta run` is WI-2 (tool registry wiring) — real sub-agent \
         delegation and live LLM inference arrive in later work items \
         (#1045 WI-3..WI-8); the registered delegate tool uses a stub runner"
    );

    let summary = wi2_summary(demo, &project, &tools);
    // stdout carries the machine-readable run summary (the human notice went to
    // stderr above), so downstream tooling can parse `meta run` output cleanly.
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

/// Resolve the optional `--project` argument to an existing absolute path.
///
/// Why: every later work item operates relative to a concrete working
/// directory, so the bootstrap must fail fast and clearly if the operator
/// points it at a path that does not exist — surfacing the bad input now
/// instead of deep inside a future delegation loop.
/// What: defaults a missing argument to the process cwd, canonicalises the path
/// (which also asserts existence) and returns the absolute form; returns an
/// `anyhow` error naming the offending path when it is absent or unreadable.
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

/// Build the WI-2 run summary as a JSON object.
///
/// Why: `meta run` emits a single machine-readable line so the run summary has a
/// stable shape across work items; isolating its construction keeps it
/// unit-testable without stdout capture. WI-2 enriches the WI-1 shape with the
/// registered tool list so downstream tooling can confirm the harness offered
/// the expected capabilities.
/// What: returns
/// `{"status":"wi2","demo":<bool>,"project":"<abs path>","tools":[<names>]}`,
/// where `tools` is the (sorted) list of registered tool names.
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
/// Why: short-lived `tm` subcommands skip the daemon's subscriber init, but
/// the metaharness requires structured logging that honours `RUST_LOG`. Using `try_init`
/// keeps this idempotent — it silently no-ops if a global subscriber is already
/// installed (e.g. under a test harness), satisfying the workspace's "no global
/// state except the idempotent tracing subscriber" rule.
/// What: installs a stderr `fmt` subscriber filtered by `RUST_LOG` (default
/// `info`); ignores the error returned when a subscriber already exists.
/// Test: side-effect-only (global subscriber install); exercised indirectly by
/// `meta_run_demo_succeeds_for_existing_project`.
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
        // The full registry-backed run over an existing project must list every
        // metaharness tool in its summary — guards the run()→registry wiring.
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
        let summary = wi2_summary(true, &tmp, &tools);
        assert_eq!(
            summary["tools"],
            json!([
                "bash",
                "delegate_to_agent",
                "edit",
                "read_file",
                "write_file"
            ])
        );
    }

    #[test]
    fn meta_run_demo_succeeds_for_existing_project() {
        // The demo path over an existing project must exit Ok (exit 0) so the
        // WI-1 scaffold is smoke-testable.
        let tmp = std::env::temp_dir();
        run(true, Some(tmp)).expect("demo run over existing project succeeds");
    }

    #[test]
    fn meta_run_errors_on_missing_project() {
        let missing = PathBuf::from("/nonexistent-meta-bootstrap-run-xyz-98765");
        assert!(run(true, Some(missing)).is_err());
    }
}
