//! `tm launch --twin`: arm the `claude` a launch starts for supervisor-twin
//! mode (#8878, ruling D1).
//!
//! Why: twin mode needs an arming the session cannot give itself. `tm launch
//! --twin` is that arming: it records the one `claude` process it started, by
//! PID and start time, in `~/.trusty-mpm/twin/armed/`.
//! What: [`precheck`] refuses `--twin` before any tmux session exists unless
//! the user-level grant holds for the session directory; [`refuse_reattach`]
//! and [`refuse_live_checkout`] refuse `--twin` where this command starts no
//! `claude` of its own; [`arm`] writes the record once the `claude` PID is known.
//! Arming failure never stops the launch: the session runs as a plain
//! supervisor, which is what the hook concludes without a record.
//! Test: `cli_parses_launch_with_twin`; the record and grant logic in
//! `core/twin_identity_tests.rs` (`an_arming_record_round_trips`,
//! `a_supervisor_without_the_twin_grant_is_not_twin`).

use std::path::{Path, PathBuf};

use trusty_mpm::core::twin_arming;
use trusty_mpm::core::twin_identity;

/// Refuse `--twin` unless `project_dir` holds the user-level twin grant.
///
/// Why: a launch that asked for twin mode and cannot get it must say so
/// before the operator starts working, not leave them in a plain supervisor.
/// What: reads `~/.trusty-mpm/config.toml` strictly and runs
/// [`twin_identity::check_grant`] — the check the hook runs. Returns the
/// `~/.trusty-mpm` root the record will be written under.
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
