//! The launch-time `statusLine` guarantee, across both settings tiers (#7617).
//!
//! Why: `session_launch::mod` is at the 500-SLOC production cap, and #7617 adds
//! a second tier to the one step every launch and every resume runs
//! unconditionally. The two entry points and their home resolution are cohesive
//! and have exactly one caller shape, so they move out together rather than
//! pushing an unrelated part of `mod.rs` over the line.
//! What: [`ensure_status_line`] (ambient user tier) and [`ensure_status_line_in`]
//! (named user tier), both re-exported from `session_launch`.
//! Test: `ensure_status_line_in_seeds_both_tiers`,
//! `ensure_status_line_in_without_a_user_tier_still_seeds_the_project` in
//! `session_launch::tests`.

use std::path::{Path, PathBuf};

use super::settings;
use crate::core::session_launch::PrepError;
use crate::core::statusline_settings::{StatuslineWrite, ensure_statusline_entry_in};

/// Defensively (re)write the `tm statusline` config for an EXISTING session
/// workspace, without re-running the full preparation pipeline.
///
/// Why (issue #1913): sessions spawned via the pre-fix in-project worktree path
/// never ran `prepare_session_with_repo_url` at all, so their on-disk
/// `.claude/settings.json` may be permanently missing the `statusLine` key, and
/// nothing else in the launch path ever backfills it. Re-running the FULL prep
/// pipeline (agent/skill redeploy, CLAUDE.md merge, MCP injection) on every
/// resume is riskier than necessary here — those steps are not all confirmed
/// idempotent under a resumed (not freshly-provisioned) workspace — so this
/// exposes ONLY the one step `write_status_line` itself documents as safe to
/// call unconditionally (it never clobbers a genuine user customization). The
/// resume path calls this defensively so a session stuck in the pre-#1913
/// broken state self-heals the next time it is resumed, without the broader
/// blast radius of a full re-prep. As of #1914, the same call ALSO upgrades a
/// stale bare `tm`/`trusty-mpm statusline` command (the pre-#1914 default,
/// which silently fails to render under a minimal `PATH`) to the resolved
/// absolute path — the two self-heal concerns share this one entry point
/// rather than growing a second, duplicate resume hook.
/// What: [`ensure_status_line_in`] against the ambient user tier.
/// Test: the underlying idempotency and path-resolution guarantees are covered
/// by `write_status_line_injects_when_absent` / `write_status_line_skips_when_already_set`
/// / `write_status_line_preserves_user_config` / `write_status_line_heals_stale_tm_default`
/// / `write_status_line_heals_stale_trusty_mpm_default` in `session_launch`'s
/// test file; `resume_managed_backfills_missing_status_line` in
/// `tests/session_manager_mvp.rs` covers the `resume_managed` call site.
pub fn ensure_status_line(project_dir: &Path) -> Result<(), PrepError> {
    // #7617: the user tier too. Claude Code merges user, project and local
    // settings, and a session launched outside the managed driver reads the
    // user tier — which, before this, had no writer at all, so a fresh install
    // that never ran a managed launch got no `💸` segment and nothing said why.
    ensure_status_line_in(project_dir, user_settings_path().as_deref())
}

/// [`ensure_status_line`] against a caller-named user-tier settings file.
///
/// Why (#7617): the user tier resolves from the process home directory, which
/// this crate's lib tests would otherwise have to `#[serial]`-guard with a
/// process-global `$HOME` write. Naming the path is what makes the second tier
/// assertable from a tempdir.
/// What: the project tier through `settings::write_status_line`, then the same
/// rule applied to `user_settings` when one is supplied. The user-tier write is
/// BEST-EFFORT and never fails the launch: a corrupt or unwritable
/// `~/.claude/settings.json` is the operator's to fix, and refusing to start a
/// session over a status bar inverts the priority (the same ruling
/// `runtime::claude_code`'s compiled-prompt write follows).
/// Test: `ensure_status_line_in_seeds_both_tiers`,
/// `ensure_status_line_in_without_a_user_tier_still_seeds_the_project`.
pub fn ensure_status_line_in(
    project_dir: &Path,
    user_settings: Option<&Path>,
) -> Result<(), PrepError> {
    settings::write_status_line(project_dir)?;
    if let Some(path) = user_settings
        && let StatuslineWrite::Refused(reason) = ensure_statusline_entry_in(path)
    {
        tracing::warn!(
            path = %path.display(),
            %reason,
            "could not seed or repair the user-tier statusLine entry (non-fatal)"
        );
    }
    Ok(())
}

/// `~/.claude/settings.json`, when a home directory resolves.
///
/// What: the user tier Claude Code reads for every session, managed or not.
/// `None` in a stripped environment with no home, which seeds nothing.
fn user_settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".claude").join("settings.json"))
}
