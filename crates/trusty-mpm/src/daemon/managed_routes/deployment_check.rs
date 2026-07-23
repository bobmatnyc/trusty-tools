//! Deployment-completeness self-check for `daemon::managed_routes::lifecycle`
//! (issue #2158, softened non-blocking by #2172).
//!
//! Why: split out of `lifecycle.rs` (issue #3726 review follow-up) purely to
//! keep `lifecycle.rs` — a production file already grandfathered on the
//! `.line-cap-allowlist.tsv` SLOC ratchet, with its frozen budget
//! deliberately never allowed to grow — from growing further. This mirrors
//! the exact motivation that already split `lifecycle_tests.rs` out of
//! `lifecycle.rs`'s inline test module. Pure code motion — no behavior
//! change; every item below is verbatim from `lifecycle.rs`.
//! What: [`ensure_deployment_complete`] (validate-then-auto-repair a
//! workspace's `.claude/` payload before handoff), the
//! [`warn_if_no_persona_carrier`] diagnostic self-check it runs, and the pure
//! [`carrier_reachable`] predicate that backs it.
//! Test: `ensure_deployment_complete_*`/`carrier_reachable_*`/
//! `warn_if_no_persona_carrier_*` remain in `lifecycle_tests.rs` (a child
//! module of `lifecycle`, which re-imports these via `use
//! deployment_check::*` — `use super::*` there still resolves the bare
//! names unchanged).

use tracing::{info, warn};

use crate::session_manager::ManagedSessionId;

/// Validate a workspace against the canonical bundled roster before handing
/// the session to the operator, auto-repairing first when gaps are found
/// (issue #2158).
///
/// Why: `prepare_session_inner`'s roster/output-style/hooks steps are already
/// best-effort/non-fatal (issue #2149) so a session always launches carrying
/// SOME identity — but "launches" is not the same as "launches complete". A
/// worktree whose `.claude/` payload came up incomplete (missing agents, a
/// stripped `settings.json`, no ownership manifest — see #2158) could
/// otherwise silently reach the operator. This function surfaces that gap:
/// validate, and if incomplete, re-run the deploy pipeline once via
/// [`crate::core::deploy_validate::validate_and_repair`] (which reuses
/// [`crate::core::session_launch::prepare_session_with_repo_url`] — the exact
/// #2149 pipeline, no parallel repair implementation), then re-validate.
/// **Non-blocking as of #2172 (P0):** every `spawn_managed_*`/`resume_managed`
/// call site now treats `Err` as a `tracing::warn!`-only diagnostic and always
/// proceeds to `adapter.spawn`/`adapter.spawn_resume` regardless of the
/// result. The original #2158 contract — skip the runtime launch and mark the
/// record errored on `Err` — turned out to be unsafe to wire as a hard gate:
/// the validator over-reports INCOMPLETE (#2171), so the gate was aborting
/// `adapter.spawn` on effectively every new/restarted managed session,
/// leaving the pane at a bare shell. This function's return type is
/// deliberately still `Result<(), String>` (callers/tests still want the
/// pass/fail detail to log or assert on) — it is the CALLERS' responsibility
/// to never let that `Err` skip the launch. Do not reintroduce an early
/// return/`mark_errored` on this `Err` at any call site without first fixing
/// #2171 and re-litigating whether a hard gate is safe.
/// What: no-ops (`Ok(())`) when `workspace` is the adopted-session sentinel
/// `/unknown` or does not exist on disk — an unresolved workspace has nothing
/// to validate; that case is handled separately by the `reconcile_on_boot`
/// adopted-session fix, not here. Otherwise delegates to `validate_and_repair`
/// using the caller-resolved `fw` (production call sites pass
/// [`crate::core::paths::FrameworkPaths::for_managed_workspace`]`(workspace)`;
/// tests inject a hermetic [`crate::core::paths::FrameworkPaths::under`]).
/// `Ok(())` when the workspace is (or becomes) complete; `Err(detail)` naming
/// every residual gap otherwise.
/// Test: `ensure_deployment_complete_noops_for_unknown_workspace`,
/// `ensure_deployment_complete_ok_when_already_complete`,
/// `ensure_deployment_complete_does_not_abort_when_no_carrier_reachable`
/// (`lifecycle_tests.rs`) cover this function directly; #2172's
/// non-blocking-call-site contract (every `spawn_managed_*`/`resume_managed`
/// site logs `Err` and still proceeds to `adapter.spawn`) is asserted at
/// each of those call sites, not here.
pub(super) fn ensure_deployment_complete(
    fw: &crate::core::paths::FrameworkPaths,
    workspace: &std::path::Path,
    repo_url: Option<&str>,
    session_id: &ManagedSessionId,
) -> Result<(), String> {
    if workspace == std::path::Path::new("/unknown") || !workspace.is_dir() {
        return Ok(());
    }
    let outcome = crate::core::deploy_validate::validate_and_repair(fw, workspace, repo_url);
    // Warn-only carrier-reachability self-check (issue #2231) — see its own
    // doc comment. Runs regardless of the completeness verdict below and can
    // NEVER turn this `Ok` branch into an `Err`; it only logs.
    warn_if_no_persona_carrier(&outcome.after.gaps, workspace, session_id);
    if outcome.before.is_complete() {
        return Ok(());
    }
    if outcome.is_complete() {
        info!(
            id = %session_id,
            gaps = outcome.before.gaps.len(),
            "deployment validation: auto-repair closed all gaps before handoff"
        );
        return Ok(());
    }
    let detail: Vec<String> = outcome.after.gaps.iter().map(|g| g.describe()).collect();
    Err(format!(
        "deployment incomplete after auto-repair ({} gap(s) remain): {}",
        detail.len(),
        detail.join("; ")
    ))
}

/// Warn-only self-check: is at least one delegation-persona CARRIER reachable
/// under the daemon path's `--setting-sources project,local` posture (issue
/// #2231)?
///
/// Why: `--setting-sources project,local` (see
/// `core::model_inject::SETTING_SOURCES_FLAG`) restricts the launched
/// `claude` to the project+local tiers and EXCLUDES the `user` tier that
/// `CLAUDE_CONFIG_DIR` relocates to (see `core::managed_config`'s module doc,
/// "WHICH LAYER ACTUALLY LOADS THE ROSTER") — so the PM's identity survives
/// ONLY if a project-tier carrier is reachable: either the deployed
/// `trusty-mpm` output-style file (`settings.json`'s `outputStyle` resolving
/// to a real file under `.claude/output-styles/`), or the per-workspace
/// instructions stash (`<workspace>/.trusty-mpm/last-instructions.md`,
/// written by `session_launch::prepare_session_inner` and injected via
/// `--append-system-prompt-file`). This is a DIAGNOSTIC ONLY, mirroring the
/// #2172/98b994c3 lesson this very function already embodies
/// (`ensure_deployment_complete` was softened from a hard gate to a
/// non-blocking warn by #2172, commit 98b994c3) — over-reporting "no carrier
/// reachable" must NEVER abort a real launch, so this only logs; it cannot
/// fail this function or any caller, and it never returns a value.
/// What: logs `tracing::warn!` with an actionable message (naming the missing
/// carriers and how they are normally wired) when [`carrier_reachable`]
/// returns `false`; a no-op otherwise.
/// Test: `carrier_reachable_*` cover the pure predicate directly;
/// `ensure_deployment_complete_does_not_abort_when_no_carrier_reachable`
/// asserts this self-check never turns the caller's `Ok` into an `Err`.
pub(super) fn warn_if_no_persona_carrier(
    gaps: &[crate::core::deploy_validate::DeploymentGap],
    workspace: &std::path::Path,
    session_id: &ManagedSessionId,
) {
    if carrier_reachable(gaps, workspace) {
        return;
    }
    warn!(
        id = %session_id,
        workspace = %workspace.display(),
        "deployment self-check: no delegation-persona carrier reachable under \
         --setting-sources project,local (no project-tier output-style file \
         resolved from settings.json's outputStyle, and no \
         .trusty-mpm/last-instructions.md prompt stash) — the launched PM may \
         be missing its identity/instructions carrier; this is diagnostic only \
         and does not block the launch (issue #2231). Re-run `tm doctor` or \
         `tm repair` against this workspace to re-provision the .claude/ payload."
    );
}

/// Pure predicate: is at least one delegation-persona carrier reachable?
///
/// Why: isolated from the logging side effect in
/// [`warn_if_no_persona_carrier`] so the decision itself is directly
/// unit-testable — see that function's doc for the full carrier-reachability
/// rationale.
/// What: `true` when `gaps` contains NONE of the output-style-related
/// [`crate::core::deploy_validate::DeploymentGap`] variants
/// (`OutputStyleKeyMissing`, `OutputStyleUnknownId`, `OutputStyleFileMissing`)
/// — the output-style carrier is intact — OR
/// `<workspace>/.trusty-mpm/last-instructions.md` exists and is non-empty (the
/// prompt-file carrier). `false` only when NEITHER carrier is reachable.
/// Test: `carrier_reachable_true_when_no_output_style_gap`,
/// `carrier_reachable_true_when_prompt_file_present_despite_style_gap`,
/// `carrier_reachable_false_when_neither_carrier_present`.
pub(super) fn carrier_reachable(
    gaps: &[crate::core::deploy_validate::DeploymentGap],
    workspace: &std::path::Path,
) -> bool {
    use crate::core::deploy_validate::DeploymentGap;

    let output_style_ok = !gaps.iter().any(|g| {
        matches!(
            g,
            DeploymentGap::OutputStyleKeyMissing
                | DeploymentGap::OutputStyleUnknownId(_)
                | DeploymentGap::OutputStyleFileMissing(_)
        )
    });
    if output_style_ok {
        return true;
    }
    workspace
        .join(".trusty-mpm")
        .join("last-instructions.md")
        .metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}
