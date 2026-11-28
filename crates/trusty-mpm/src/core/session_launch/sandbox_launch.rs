//! The one launch-time entry to the per-role sandbox settings (#10102).
//!
//! Why: #8928 step 6 — `tm launch`/`tm connect`, the daemon's managed spawn,
//! the Architect launch and the divert worker each start a `claude`, and each
//! must start it only inside the sandbox its role asked for. One entry keeps
//! the refusal rules identical on every path.
//! What: [`sandbox_role_for`] maps a launch profile to its role,
//! [`launch_sandbox_inputs`] resolves the paths a policy names, and
//! [`apply_launch_sandbox`] is the launch entry. #12077: enforcement is off on
//! every launch path, so it writes nothing; the policy writer stays in
//! `enforce_launch_sandbox` for the slice-2 opt-in.
//! Test: `a_default_launch_writes_no_sandbox_key_12077`,
//! `a_launch_with_no_config_dir_is_not_refused_12077`,
//! `a_launch_with_no_config_dir_is_refused`,
//! `a_launch_applies_and_verifies_the_policy`.

use std::path::Path;

use super::{SandboxInputs, SandboxRole, SandboxSettingsError, write_sandbox_settings};
use crate::core::session_profile::SessionProfile;

/// The sandbox role of a launch with `profile`: the Architect profile is the
/// unsandboxed Architect, every other profile is a PM.
pub fn sandbox_role_for(profile: SessionProfile) -> SandboxRole {
    if profile.is_architect() {
        SandboxRole::Architect
    } else {
        SandboxRole::Pm
    }
}

/// The paths a policy for `workspace` under `home` names.
///
/// What: the workspace's private tmux socket (#10101), the daemon's
/// `trusty-mpm.sock` when it resolves, an absolute `CARGO_TARGET_DIR`, and
/// the sccache socket when `home`'s config opts in (`build.sccache: true`).
/// A config that cannot be read leaves sccache off.
/// Test: `a_launch_applies_and_verifies_the_policy`.
pub fn launch_sandbox_inputs(workspace: &Path, home: &Path) -> SandboxInputs {
    let mut inputs = SandboxInputs::new(workspace, home);
    let server = crate::core::tmux_server::workspace_server_name(workspace);
    inputs.tmux_socket = Some(crate::core::tmux_server::server_socket_path(
        &crate::core::tmux_server::ambient_socket_dir(),
        &server,
    ));
    inputs.mpm_socket = crate::client::http_client::resolve_daemon_socket().ok();
    inputs.cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute());
    inputs.sccache_socket = crate::core::sccache_socket::sccache_requested(home)
        .unwrap_or(false)
        .then(|| crate::core::sccache_socket::socket_path_at(home));
    inputs
}

/// The launch-time sandbox step: a no-op while enforcement is off (#12077).
///
/// Why: #12077 slice 1 — the Claude Code sandbox enforcement is off on every
/// launch path until an opt-in exists.
/// What: returns `Ok` before any write or refusal. It writes no project tier,
/// no user tier and no role record, and refuses no config dir. Every launch
/// path (`tm launch`/`tm connect`, `tm run`, the managed spawn, resume and
/// relaunch, the divert worker) reaches the sandbox only through here.
///
/// # Errors
///
/// None while enforcement is off; the `Result` keeps the callers unchanged.
///
/// Test: `a_default_launch_writes_no_sandbox_key_12077`,
/// `a_launch_with_no_config_dir_is_not_refused_12077`.
pub fn apply_launch_sandbox(
    role: SandboxRole,
    inputs: &SandboxInputs,
    _config_dir: Option<&Path>,
) -> Result<(), SandboxSettingsError> {
    tracing::debug!(
        ?role,
        workspace = %inputs.workspace.display(),
        "#12077: Claude Code sandbox enforcement is off; no sandbox settings written"
    );
    Ok(())
}

/// Write and verify `role`'s sandbox policy: the pre-#12077 launch step.
///
/// Why: #10102 criteria 1 and 2 — a policy that does not verify must refuse
/// the launch, and the user tier must never be the operator's own
/// `~/.claude`, which every other Claude Code on the host reads.
/// What: refuses when `config_dir` is `None` (the spawn sets no
/// `CLAUDE_CONFIG_DIR`, so Claude Code would read `~/.claude`), relative, or
/// `<home>/.claude`; otherwise [`write_sandbox_settings`], which verifies.
/// #12077: unreachable from launch until the slice-2 opt-in calls it.
///
/// # Errors
///
/// [`SandboxSettingsError::Refused`] for an unusable config dir or a refused
/// read-back; any write error of [`write_sandbox_settings`].
///
/// Test: `a_launch_with_no_config_dir_is_refused`,
/// `a_launch_into_the_operators_claude_dir_is_refused`,
/// `a_launch_applies_and_verifies_the_policy`,
/// `a_verify_failure_refuses_the_launch`.
#[allow(dead_code)] // #12077: slice 2 calls this behind the opt-in.
pub(super) fn enforce_launch_sandbox(
    role: SandboxRole,
    inputs: &SandboxInputs,
    config_dir: Option<&Path>,
) -> Result<(), SandboxSettingsError> {
    let Some(dir) = config_dir else {
        return Err(SandboxSettingsError::Refused(
            "#8928 sandbox settings: this launch has no tm-owned Claude config dir \
             (`CLAUDE_CONFIG_DIR` is unset), so the sandbox's user-level settings would land \
             in your own `~/.claude`. Start the session with `tm launch` or `tm connect`, which \
             use the tm-managed config dir, or export `CLAUDE_CONFIG_DIR` to a tm-owned \
             directory; `tm doctor` names it"
                .to_owned(),
        ));
    };
    let operators = inputs.home.join(".claude");
    if !dir.is_absolute() || dir == operators {
        return Err(SandboxSettingsError::Refused(format!(
            "#8928 sandbox settings: the Claude config dir `{}` is not a tm-owned absolute \
             directory; tm never writes the sandbox into `{}`. Point `CLAUDE_CONFIG_DIR` at the \
             tm-managed config dir (`tm doctor` names it)",
            dir.display(),
            operators.display()
        )));
    }
    write_sandbox_settings(role, inputs, dir)
}

#[cfg(test)]
#[path = "sandbox_launch_tests.rs"]
mod tests;
