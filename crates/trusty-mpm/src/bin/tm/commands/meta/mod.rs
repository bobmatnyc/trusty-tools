//! `meta` command handler — standalone metaharness driving a real Claude Code
//! session (#1045 epic; #1049 WI-A; #1051 WI-B+C).
//!
//! Why: the re-scoped M1 POC (epic #1045) launches a REAL `claude` CLI session
//! through trusty-mpm's EXISTING machinery — NOT an in-process trusty-code
//! orchestrator (Claude Code handles sub-agent delegation natively via its Task
//! tool, so no custom delegate runner is needed). `meta run --project <dir>`
//! deploys the custom instructions and launches a real `claude` tmux session
//! rooted at that dir (WI-A, #1049); `meta run --demo` additionally attaches a
//! bundled task that writes `hello_metaharness.txt`, polls for the session to
//! exit, verifies the artifact, prints a structured report, and exits 0 on
//! success / non-zero on failure/timeout (WI-B+C, #1051). The harness runs
//! standalone — the daemon is NOT required.
//! What: [`meta`] dispatches [`MetaAction`]; [`run`] resolves `--project`, builds
//! a daemon-free [`SessionManager`](trusty_mpm::session_manager::SessionManager),
//! and calls [`launch::launch_and_wait`]; for `--demo` it then runs
//! [`verify::verify_artifact`] and fails the process on a non-pass verdict,
//! emitting the captured tmux transcript for diagnostics. The launch/poll lives
//! in `launch.rs` and the artifact check in `verify.rs`; both factor their
//! decision logic into pure, unit-testable functions (the live end-to-end is
//! `#[ignore]`-gated as the #1053 follow-up).
//! Test: `meta_*` unit tests in this module's `tests` block; pure poll/verify
//! unit tests in `launch`/`verify`; CLI parsing in `tests.rs`.

mod launch;
mod verify;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::json;
use tracing::{error, info};

use crate::cli::MetaAction;

/// Default session-exit poll budget, in seconds, for `meta run`.
///
/// Why: a stuck `claude` session must never hang the command forever; a sensible
/// default bounds the wait while `--timeout-secs` lets operators override it for
/// slower tasks (#1051). Centralising the value keeps the CLI help text, the
/// handler, and any tooling in lockstep.
/// What: 120 seconds (the #1051 default).
/// Test: `meta_default_timeout_is_120s`.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// `meta` subcommand dispatcher — route a parsed [`MetaAction`] to its handler.
///
/// Why: mirrors the other `tm` command groups by keeping `main`'s match arm thin
/// and folding verb dispatch into the module that owns the verbs.
/// What: matches the [`MetaAction`] and forwards `meta run` to [`run`] (async, so
/// it can drive the live launch + poll without a nested runtime).
/// Test: covered by the handler unit tests via the `Run` arm; CLI parse
/// round-trips live in `tests.rs`.
pub(crate) async fn meta(action: MetaAction) -> anyhow::Result<()> {
    match action {
        MetaAction::Run {
            demo,
            project,
            no_provision,
            timeout_secs,
        } => run(demo, project, no_provision, timeout_secs).await,
    }
}

/// Execute one `meta run` invocation.
///
/// Why: this is the harness's primary entry point (#1049/#1051). It launches a
/// real `claude` session for the project dir using trusty-mpm's existing launch
/// machinery; with `--demo` it attaches a checkable task and verifies the
/// resulting artifact so a run's success is decidable without operator input.
/// What: initialises stderr tracing; resolves `--project`; computes the poll
/// budget (`--timeout-secs` or [`DEFAULT_TIMEOUT_SECS`]); builds a daemon-free
/// session manager rooted under `<project>/.trusty-mpm/meta-sessions`; for
/// `--demo` it derives a run id, generates the bundled task, launches + waits,
/// then verifies the artifact — printing a structured JSON report to stdout and
/// returning `Err` (non-zero exit) on a non-pass verdict or timeout; without
/// `--demo` it launches and waits with no task and reports how the session ended.
/// `no_provision` is currently a NO-OP: the POC always operates on the local
/// `--project` dir in place, so there is no provisioning/clone step to skip. The
/// flag is accepted to make that intent explicit on the CLI and to reserve the
/// seam for a future provisioned/clone path (`--no-provision=false`); passing it
/// either way changes nothing today.
/// Test: `meta_run_dir_paths_are_project_scoped`,
/// `meta_resolve_project_*`, `meta_run_id_is_unique`; the live launch is covered
/// by the `#[ignore]` end-to-end test (#1053).
pub(crate) async fn run(
    demo: bool,
    project: Option<PathBuf>,
    no_provision: bool,
    timeout_secs: Option<u64>,
) -> anyhow::Result<()> {
    init_meta_tracing();
    let project = resolve_project(project)?;
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

    // `no_provision` is a documented no-op today: the POC always uses the local
    // `--project` dir in place, so there is never a provisioning/clone step to
    // skip. This explicit branch makes the no-op unmistakable (no silent surprise
    // for `--no-provision=false`) and marks the seam where a future provisioned /
    // git-clone path would live. M1 POC hardening follow-up.
    if !no_provision {
        // Future: provision an isolated workspace (e.g. git clone) instead of
        // operating on the local dir in place. No behavioural difference today.
    }

    info!(
        demo,
        no_provision,
        project = %project.display(),
        timeout_secs = timeout.as_secs(),
        "meta run: launching a real Claude Code session via the existing machinery"
    );

    let state_dir = launch::state_dir(&project);
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create state dir: {}", state_dir.display()))?;
    let mgr = launch::new_session_manager(&state_dir).await?;

    if demo {
        run_demo(&mgr, &project, timeout).await
    } else {
        run_plain(&mgr, &project, timeout).await
    }
}

/// Launch a session for `project` with no task and report how it ended.
///
/// Why: a bare `meta run` is the WI-A smoke path (#1049) — it proves the launch
/// machinery wires up and brings a real `claude` session online rooted at the
/// project dir, without the demo's verification step.
/// What: calls [`launch::launch_and_wait`] with no task, prints a JSON summary of
/// the [`LaunchOutcome`](launch::LaunchOutcome), and returns `Ok(())` regardless
/// of how the session ended (a bare run makes no pass/fail claim).
/// Test: side-effect/IO-heavy (live `claude`); covered by the `#[ignore]` test.
async fn run_plain(
    mgr: &trusty_mpm::session_manager::SessionManager,
    project: &Path,
    timeout: Duration,
) -> anyhow::Result<()> {
    let report = launch::launch_and_wait(mgr, project, None, timeout).await?;
    let summary = json!({
        "status": "launched",
        "demo": false,
        "project": project.display().to_string(),
        "tmux": report.tmux_name,
        "outcome": report.outcome.status(),
    });
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

/// Run the demo: launch with a checkable task, poll, and verify the artifact.
///
/// Why: this is the WI-B+C payoff (#1051) — proof the launched session did real
/// work. Verifying a known file's content makes the run's success deterministic
/// and machine-checkable; on failure the captured tmux transcript is surfaced so
/// the operator can diagnose what the session actually did (#1052 observability).
/// What: derives a run id, builds the bundled task via [`verify::demo_task`],
/// launches + waits via [`launch::launch_and_wait`], then checks the artifact
/// with [`verify::verify_artifact`]. Prints a structured JSON report to stdout.
/// On a [`VerifyOutcome::Pass`](verify::VerifyOutcome::Pass) it returns `Ok(())`
/// (exit 0); on any other verdict (or a timed-out launch with a non-pass verdict)
/// it logs the transcript to stderr and returns `Err` so `main` exits non-zero.
/// Test: side-effect/IO-heavy; the pure verify/poll logic is covered by
/// `verify::tests` / `launch::tests`, the live path by the `#[ignore]` test.
async fn run_demo(
    mgr: &trusty_mpm::session_manager::SessionManager,
    project: &Path,
    timeout: Duration,
) -> anyhow::Result<()> {
    let run_id = run_id();
    let expected = verify::expected_content(&run_id);
    let task = verify::demo_task(&run_id);
    info!(run_id = %run_id, "meta run --demo: bundled task prepared");

    let report = launch::launch_and_wait(mgr, project, Some(&task), timeout).await?;
    let verdict = verify::verify_artifact(project, &expected);

    let summary = json!({
        "status": "demo",
        "demo": true,
        "project": project.display().to_string(),
        "run_id": run_id,
        "tmux": report.tmux_name,
        "launch_outcome": report.outcome.status(),
        "artifact": verify::DEMO_ARTIFACT,
        "verdict": verdict.status(),
        "pass": verdict.is_pass(),
    });
    println!("{}", serde_json::to_string(&summary)?);

    if verdict.is_pass() {
        info!(
            run_id = %run_id,
            "meta run --demo: PASS — session wrote and verified {}",
            verify::DEMO_ARTIFACT
        );
        Ok(())
    } else {
        // Surface the pane transcript for diagnostics (#1052) before failing.
        error!(
            run_id = %run_id,
            verdict = verdict.status(),
            launch_outcome = report.outcome.status(),
            "meta run --demo: FAIL — captured tmux transcript follows:\n{}",
            report.transcript
        );
        bail!(
            "meta run --demo failed: artifact verdict '{}' (launch {}); see the captured \
             transcript above for diagnostics",
            verdict.status(),
            report.outcome.status()
        )
    }
}

/// Process-monotonic counter mixed into every [`run_id`] to guarantee uniqueness.
///
/// Why: deriving the id from the wall clock alone is not collision-free — two
/// back-to-back calls within the same millisecond (or on a low-resolution clock)
/// would produce identical ids, letting a stale artifact masquerade as the
/// current run's. A per-process counter that strictly increases on every call
/// makes the id unique by construction, with no reliance on clock resolution.
/// What: an `AtomicU64`, bumped with `Relaxed` ordering on each `run_id` call
/// (ordering across threads is irrelevant — we only need a distinct value, not a
/// happens-before relationship).
/// Test: `meta_run_id_is_unique` (two ids drawn with no delay still differ).
static RUN_ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Generate a short, unique id for one demo run.
///
/// Why: embedding a per-run id in the demo artifact body lets verification prove
/// THIS run produced the file (not a stale one from a prior run). Anchoring it to
/// the wall clock keeps it sortable/human-meaningful, while a process-monotonic
/// counter suffix guarantees uniqueness even for two calls in the same
/// millisecond (so the id format stays purely numeric for downstream checks).
/// What: returns `"<unix-millis><6-digit zero-padded counter>"` (the counter
/// strictly increases per call); falls back to `"0"`-millis if the clock is
/// before the epoch (which cannot happen for `SystemTime::now`).
/// Test: `meta_run_id_is_unique` (two ids differ with no delay),
/// `meta_run_id_is_numeric`.
pub(crate) fn run_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // `Relaxed` is sufficient: we need a distinct value per call, not ordering.
    let seq = RUN_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Zero-pad the counter to 6 digits so it cannot blur into the millis prefix
    // for typical run counts while keeping the whole id numeric.
    format!("{millis}{seq:06}")
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

/// Initialise stderr tracing for the short-lived `meta run` invocation.
///
/// Why: short-lived `tm` subcommands skip the daemon's subscriber init, but the
/// metaharness requires structured logging that honours `RUST_LOG`. `try_init`
/// keeps this idempotent — it silently no-ops if a subscriber already exists.
/// Logging goes to STDERR so stdout carries only the command's structured result.
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
    fn meta_default_timeout_is_120s() {
        assert_eq!(DEFAULT_TIMEOUT_SECS, 120);
    }

    #[test]
    fn meta_run_id_is_numeric() {
        let id = run_id();
        assert!(
            id.chars().all(|c| c.is_ascii_digit()),
            "run id must be numeric, got: {id}"
        );
    }

    #[test]
    fn meta_run_id_is_unique() {
        // Uniqueness is guaranteed by the process-monotonic counter, NOT by clock
        // resolution — so two ids drawn with no delay must still differ. (The old
        // test slept 2ms and relied on the wall clock ticking, which could flake on
        // loaded / low-resolution CI.)
        let a = run_id();
        let b = run_id();
        assert_ne!(a, b, "run ids must be unique across back-to-back calls");
    }
}
