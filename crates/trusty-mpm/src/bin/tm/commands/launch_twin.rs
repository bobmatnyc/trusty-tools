//! `tm launch --twin`: arm the `claude` a launch starts for supervisor-twin
//! mode (#8878, ruling D1).
//!
//! Why: twin mode needs an arming the session cannot give itself. `tm launch
//! --twin` is that arming: it records the one `claude` process it started, by
//! PID and start time, in `~/.trusty-mpm/twin/armed/`.
//! What: [`preflight`] refuses `--twin --worktree` before `tm launch` does
//! anything; [`precheck`] refuses `--twin` before any clone or worktree is
//! provisioned unless the user-level grant holds for the managed checkout, and
//! [`confirm_placement`] refuses a session placed anywhere else;
//! [`refuse_reattach`] and [`refuse_live_checkout`] refuse `--twin` where this
//! command starts no `claude` of its own; [`arm`] writes the record once the
//! `claude` PID is known. Arming failure never stops the launch: the session
//! runs as a plain supervisor, which is what the hook concludes without a record.
//! Test: `cli_parses_launch_with_twin`,
//! `a_twin_worktree_launch_is_refused_before_anything_is_touched`,
//! `twin_preflight_refuses_only_twin_with_worktree`,
//! `twin_placement_must_be_the_checked_directory`,
//! `twin_refusals_apply_only_to_twin_launches`; the record and grant logic in
//! `core/twin_identity_tests.rs` (`an_arming_record_round_trips`,
//! `a_supervisor_without_the_twin_grant_is_not_twin`).

use std::path::{Path, PathBuf};

use trusty_mpm::core::twin_arming;
use trusty_mpm::core::twin_identity;

/// Refuse `--twin --worktree` before `tm launch` touches anything.
///
/// Why: a per-session worktree is never the granted managed checkout, so the
/// grant check would always refuse it — after the clone, worktree and branch
/// were already made and left behind (#8878 review).
/// What: `Err` exactly when both flags are set.
/// Test: `twin_preflight_refuses_only_twin_with_worktree`,
/// `a_twin_worktree_launch_is_refused_before_anything_is_touched`.
pub(crate) fn preflight(twin: bool, worktree: bool) -> anyhow::Result<()> {
    if twin && worktree {
        anyhow::bail!(
            "`--twin` refused with `--worktree`: twin mode is granted to the managed \
             checkout itself, never to a per-session worktree. Launch without `--worktree`."
        );
    }
    Ok(())
}

/// Refuse a twin session placed anywhere but the directory [`precheck`] passed.
///
/// Why: the grant is checked before provisioning, against the managed checkout
/// the placement rule should select; this confirms provisioning selected it.
/// What: `Ok` when both paths canonicalize to one directory; `Err` when they
/// differ or either cannot be canonicalized.
/// Test: `twin_placement_must_be_the_checked_directory`.
pub(crate) fn confirm_placement(checked: &Path, placed: &Path) -> anyhow::Result<()> {
    let canon = |p: &Path| {
        std::fs::canonicalize(p)
            .map_err(|e| anyhow::anyhow!("`--twin` refused: {}: {e}", p.display()))
    };
    let (checked_c, placed_c) = (canon(checked)?, canon(placed)?);
    if checked_c != placed_c {
        anyhow::bail!(
            "`--twin` refused: the session was placed in {}, not the granted {}",
            placed_c.display(),
            checked_c.display()
        );
    }
    Ok(())
}

/// Refuse `--twin` unless `project_dir` holds the user-level twin grant.
///
/// Why: a launch that asked for twin mode and cannot get it must say so
/// before the operator starts working, not leave them in a plain supervisor.
/// What: reads `~/.trusty-mpm/config.toml` strictly and runs
/// [`twin_identity::check_grant`] — the check the hook runs. Returns the
/// `~/.trusty-mpm` root the record will be written under. `tm launch` calls it
/// on the managed checkout before provisioning anything (#8878 review).
/// Test: the grant rule is `a_supervisor_without_the_twin_grant_is_not_twin`.
pub(crate) fn precheck(project_dir: &Path) -> anyhow::Result<PathBuf> {
    let root = dirs::home_dir()
        .map(|home| home.join(".trusty-mpm"))
        .ok_or_else(|| anyhow::anyhow!("`--twin` refused: the home directory is unknown"))?;
    let config = twin_arming::load_user_config_strict(&root)
        .map_err(|e| anyhow::anyhow!("`--twin` refused: the user config could not be read: {e}"))?;
    twin_identity::check_grant(project_dir, &config).map_err(|refusal| {
        anyhow::anyhow!(
            "`--twin` refused for {}: {refusal}. Twin mode needs the project in both \
             `[supervisor] projects` and `[supervisor.twin] projects` in \
             ~/.trusty-mpm/config.toml, and `profile = \"supervisor\"` in its \
             .trusty-mpm.toml (#8878).",
            project_dir.display()
        )
    })?;
    Ok(root)
}

/// Refuse `--twin` for a checkout with no origin remote, which `tm launch`
/// hands to `tm connect`; that path does not arm.
/// Test: `twin_refusals_apply_only_to_twin_launches`.
pub(crate) fn refuse_live_checkout(twin: bool) -> anyhow::Result<()> {
    if twin {
        anyhow::bail!(
            "`--twin` refused: this checkout has no origin remote, and its session starts \
             through `tm connect`, which does not arm twin mode."
        );
    }
    Ok(())
}

/// Refuse `--twin` when the launch would reattach to a running session.
/// Test: `twin_refusals_apply_only_to_twin_launches`.
pub(crate) fn refuse_reattach(twin: bool, session: &str) -> anyhow::Result<()> {
    if twin {
        anyhow::bail!(
            "`--twin` refused: session {session} is already running, and twin mode arms only \
             a claude this command starts. Stop that session, or reattach without `--twin`."
        );
    }
    Ok(())
}

/// Record the started `claude` as armed; report the outcome on stderr.
///
/// What: `claude_pid` `None` (not found in the pane) or an
/// [`twin_arming::arm_claude`] error prints a warning and leaves the session
/// unarmed.
pub(crate) fn arm(root: &Path, claude_pid: Option<u32>, project_dir: &Path) {
    let Some(pid) = claude_pid else {
        eprintln!("warning: twin mode NOT armed: the claude process could not be found");
        return;
    };
    match twin_arming::arm_claude(root, pid, project_dir) {
        Ok(record) => eprintln!(
            "twin mode armed for claude pid {} ({})",
            record.pid,
            twin_arming::record_path(root, pid).display()
        ),
        Err(e) => eprintln!("warning: twin mode NOT armed: {e}"),
    }
}
