//! The one launch-time entry to the per-role sandbox settings (#10102).
//!
//! Why: #8928 step 6 — `tm launch`/`tm connect`, the daemon's managed spawn,
//! the Architect launch and the divert worker each start a `claude`, and each
//! must start it only inside the sandbox its role asked for. One entry keeps
//! the refusal rules identical on every path.
//! What: [`sandbox_role_for`] maps a launch profile to its role,
//! [`launch_sandbox_inputs`] resolves the paths a policy names, and
//! [`apply_launch_sandbox`] writes and verifies the policy, refusing when the
//! launch has no tm-owned user-tier config dir.
//! Test: `a_launch_with_no_config_dir_is_refused`,
//! `a_launch_into_the_operators_claude_dir_is_refused`,
//! `a_launch_applies_and_verifies_the_policy`,
//! `a_verify_failure_refuses_the_launch`.

use std::path::{Path, PathBuf};

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

/// Write and verify `role`'s sandbox policy before a launch starts.
///
/// Why: acceptance criteria 1 and 2 of #10102 — a launch whose policy does not
/// verify must not start, and the user tier must never be the operator's own
/// `~/.claude`, which every other Claude Code on the host reads.
/// What: refuses when `config_dir` is `None` (the spawn sets no
/// `CLAUDE_CONFIG_DIR`, so Claude Code would read `~/.claude`), relative, or
/// `<home>/.claude`; otherwise [`write_sandbox_settings`], which verifies.
/// FAIL-CLOSED: every caller refuses its launch on `Err`.
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
pub fn apply_launch_sandbox(
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

/// The user home a launch's sandbox policy names (#10102).
///
/// Why: since #11821 P3 the framework root sits under the config base, which
/// `TRUSTY_MPM_CONFIG_DIR` or `install.toml` can move off `$HOME`; a home
/// derived from that root left the real `~/.ssh` and `~/.aws` readable.
/// What: [`resolve_launch_home`] over the process env and `dirs::home_dir()`.
/// FAIL-CLOSED: no resolvable home is an error, never a fallback.
/// Test: `a_split_config_base_sandbox_names_the_real_home`.
pub fn sandbox_launch_home() -> Result<PathBuf, SandboxSettingsError> {
    resolve_launch_home(
        std::env::var_os("TRUSTY_SANDBOX").as_deref(),
        std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).as_deref(),
        dirs::home_dir(),
    )
    .map_err(SandboxSettingsError::Refused)?
    .ok_or_else(|| {
        SandboxSettingsError::Refused(
            "#10102 sandbox settings: no home directory resolves, so the sandbox cannot name \
             `~/.ssh` or `~/.aws`; refusing the launch"
                .to_owned(),
        )
    })
}

/// The home every launch-time write goes under, with the env passed in.
///
/// Why (#11077): a sandboxed client (`TRUSTY_SANDBOX=1`, the env
/// `scripts/sandbox_daemon.sh` sets up) must never write or name its own
/// `$HOME`; tests must not mutate process-global env.
/// What: `sandbox` exactly `1` gives `<parent of data_override>/home`, the
/// layout `sandbox_daemon.sh` creates; an unset, relative or rootless override
/// is an error, never a fall back to `home` (Fail-Open Check). Otherwise `home`.
/// Test: `sandboxed_launch_home_is_the_sandbox_home_not_the_process_home`,
/// `sandboxed_launch_home_refuses_without_a_data_override`,
/// `unsandboxed_launch_home_is_the_process_home`.
pub fn resolve_launch_home(
    sandbox: Option<&std::ffi::OsStr>,
    data_override: Option<&std::ffi::OsStr>,
    home: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    // #11077: the sandbox flag routes every launch-time write into the sandbox.
    if sandbox != Some(std::ffi::OsStr::new("1")) {
        return Ok(home);
    }
    let data = data_override
        .filter(|raw| !raw.is_empty())
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            "TRUSTY_SANDBOX=1 but TRUSTY_DATA_DIR_OVERRIDE is not an absolute path; \
             refusing to write launch files under the real home (#11077)"
                .to_owned()
        })?;
    let root = data
        .parent()
        .filter(|parent| parent.parent().is_some())
        .ok_or_else(|| {
            format!(
                "TRUSTY_DATA_DIR_OVERRIDE={} has no sandbox dir above it; refusing to \
                 write launch files under the real home (#11077)",
                data.display()
            )
        })?;
    Ok(Some(root.join("home")))
}

#[cfg(test)]
#[path = "sandbox_launch_tests.rs"]
mod tests;
