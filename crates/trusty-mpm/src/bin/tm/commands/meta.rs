//! `meta` command handler — standalone metaharness bootstrap (#1045, WI-1).
//!
//! Why: issue #1045 builds an M1 POC metaharness that boots without the
//! trusty-mpm daemon or the `claude` CLI and drives PM → sub-agent delegation
//! in-process via trusty-code (Seam A). WI-1 is the bootstrap skeleton: it
//! stands up the `meta run` entry point — argument validation, structured
//! logging, and a placeholder run summary — so later work items (the
//! trusty-code facade, instruction loading, the orchestrator, live OpenRouter
//! inference, and transcript capture) have a wired, exercisable scaffold to
//! build on. The actual delegation loop is intentionally NOT implemented here.
//! What: [`meta`] dispatches `MetaAction`; [`run`] validates the `--project`
//! path, emits structured tracing, prints a human-readable "not yet
//! implemented" notice to stderr, and writes a JSON summary to stdout. Pure
//! helpers ([`resolve_project`], [`bootstrap_summary`]) carry the testable
//! logic so unit tests need no stdout capture or live runtime.
//! Test: `meta_*` unit tests in this module's `tests` block; CLI parsing in
//! `tests.rs` (`cli_parses_meta_run*`).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use tracing::{info, warn};

use crate::cli::MetaAction;

/// Status string stamped into the WI-1 bootstrap run summary.
///
/// Why: the summary `status` is a magic string consumed by tests (and,
/// eventually, by tooling that scrapes `meta run` output); centralising it
/// keeps the producer and every assertion in lockstep.
/// What: the literal `"bootstrap"` — signalling that this run exercised only the
/// WI-1 scaffold and performed no real delegation.
/// Test: `meta_bootstrap_summary_reports_status` asserts the emitted value.
pub(crate) const STATUS_BOOTSTRAP: &str = "bootstrap";

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

/// Execute one `meta run` invocation (WI-1: bootstrap skeleton only).
///
/// Why: this is the harness's primary entry point. WI-1 must make the scaffold
/// exercisable end-to-end — validate inputs, log via the shared tracing setup,
/// and emit a machine-readable summary — without implementing the (deferred)
/// trusty-code delegation loop. Returning `Ok(())` on the happy path lets the
/// demo command exit 0 so the scaffold can be smoke-tested.
/// What: initialises stderr tracing (idempotent, honours `RUST_LOG`), resolves
/// and validates `--project` to an existing absolute path, prints a clear
/// "not yet implemented — WI-1 bootstrap" notice to stderr, and writes the
/// [`bootstrap_summary`] JSON object to stdout.
/// Test: `meta_run_demo_succeeds_for_existing_project`,
/// `meta_run_errors_on_missing_project` exercise the validation + happy paths.
pub(crate) fn run(demo: bool, project: Option<PathBuf>) -> anyhow::Result<()> {
    init_meta_tracing();

    let project = resolve_project(project)?;
    info!(
        demo,
        project = %project.display(),
        "meta run: bootstrap scaffold (WI-1) — no delegation performed yet"
    );
    warn!(
        "`tm meta run` is not yet implemented — WI-1 bootstrap only; \
         the trusty-code delegation loop, fs/bash tools, and live LLM \
         inference arrive in later work items (#1045 WI-2..WI-8)"
    );

    let summary = bootstrap_summary(demo, &project);
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

/// Build the WI-1 bootstrap run summary as a JSON object.
///
/// Why: `meta run` emits a single machine-readable line so the (eventual) run
/// summary has a stable shape from the very first work item; isolating its
/// construction keeps it unit-testable without stdout capture.
/// What: returns `{"status":"bootstrap","demo":<bool>,"project":"<abs path>"}`.
/// Test: `meta_bootstrap_summary_reports_status`,
/// `meta_bootstrap_summary_carries_demo_and_project`.
pub(crate) fn bootstrap_summary(demo: bool, project: &Path) -> serde_json::Value {
    json!({
        "status": STATUS_BOOTSTRAP,
        "demo": demo,
        "project": project.display().to_string(),
    })
}

/// Initialise stderr tracing for the short-lived `meta run` invocation.
///
/// Why: short-lived `tm` subcommands skip the daemon's subscriber init, but
/// WI-1 requires structured logging that honours `RUST_LOG`. Using `try_init`
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
    fn meta_bootstrap_summary_reports_status() {
        let summary = bootstrap_summary(true, Path::new("/tmp"));
        assert_eq!(summary["status"], STATUS_BOOTSTRAP);
    }

    #[test]
    fn meta_bootstrap_summary_carries_demo_and_project() {
        let summary = bootstrap_summary(false, Path::new("/work/p"));
        assert_eq!(summary["demo"], false);
        assert_eq!(summary["project"], "/work/p");
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
