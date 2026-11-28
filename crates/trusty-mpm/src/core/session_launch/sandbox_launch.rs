//! The one launch-time entry to the per-role sandbox settings (#10102).
//!
//! Why: #8928 step 6 — `tm launch`/`tm connect`, the daemon's managed spawn,
//! the Architect launch and the divert worker each start a `claude`, and each
//! must start it only inside the sandbox its role asked for. One entry keeps
//! the refusal rules identical on every path.
//! What: [`sandbox_role_for`] maps a launch profile to its role,
//! [`launch_sandbox_inputs`] resolves the paths a policy names, and
//! [`apply_launch_sandbox`] is the launch entry. #12077: it writes nothing on
//! any launch path, opted in or not; an opted-in launch carries the policy
//! inline on argv (`crate::core::claude_sandbox`), and the file writer in
//! `enforce_launch_sandbox` has no launch caller.
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
/// `trusty-mpm.sock` when it resolves, the [`client_daemon_sockets`]
/// (#12077), an absolute `CARGO_TARGET_DIR`, and the sccache socket when
/// `home`'s config opts in (`build.sccache: true`). A config that cannot be
/// read leaves sccache off.
/// Test: `a_launch_applies_and_verifies_the_policy`,
/// `a_launch_allows_exactly_the_client_daemon_sockets`.
pub fn launch_sandbox_inputs(workspace: &Path, home: &Path) -> SandboxInputs {
    let mut inputs = SandboxInputs::new(workspace, home);
    let server = crate::core::tmux_server::workspace_server_name(workspace);
    inputs.tmux_socket = Some(crate::core::tmux_server::server_socket_path(
        &crate::core::tmux_server::ambient_socket_dir(),
        &server,
    ));
    inputs.mpm_socket = crate::client::http_client::resolve_daemon_socket().ok();
    inputs.daemon_sockets = client_daemon_sockets();
    inputs.cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute());
    inputs.sccache_socket = crate::core::sccache_socket::sccache_requested(home)
        .unwrap_or(false)
        .then(|| crate::core::sccache_socket::socket_path_at(home));
    inputs
}

/// The daemon sockets a sandboxed role's CLI clients dial, besides
/// `trusty-mpm.sock`.
///
/// Why: #12077 — the trusty daemons are socket-only, so a sandbox that names
/// only `trusty-mpm.sock` breaks `tm memory`, `trusty-search`,
/// `trusty-review` (it dials trusty-analyze) and `ta` from Bash, while MCP
/// already reaches the same daemons.
/// What: each path from the dial-side resolver its client uses: trusty-memory,
/// trusty-search, trusty-analyze and trusty-agents. A resolver that fails
/// leaves its socket out and refuses nothing. Never trusty-code,
/// trusty-secrets, trusty-events, trusty-console, trusty-embedderd or sccache.
/// Test: `a_launch_allows_exactly_the_client_daemon_sockets`.
fn client_daemon_sockets() -> Vec<std::path::PathBuf> {
    [
        trusty_common::memory_rpc::resolve_memory_dial_socket().ok(),
        trusty_common::search_rpc::search_dial_socket().ok(),
        // No shared name constant reaches this crate; same rule as `tm tui`.
        trusty_common::daemon_socket_path_no_create("trusty-analyze").ok(),
        // #12077: no trusty-code.sock — its daemon runs agent loops unsandboxed
        // (`task.run`) and `session.permission.respond` has no caller binding.
        trusty_agents_common::health::agents_dial_socket().ok(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The launch-time sandbox step: writes no sandbox policy (#12077).
///
/// Why: #12077 — tm never writes the sandbox into a Claude settings file; an
/// opted-in launch carries it inline (`crate::core::claude_sandbox`).
/// What: runs the one-time cleanup pass
/// ([`crate::core::claude_sandbox::clean_tm_sandbox_keys_on_launch`]) over the
/// project tier, tm's user tier under `config_dir`, and the role record when
/// the launch's own `opted_in` is false; then returns `Ok`. It writes no
/// sandbox policy into any tier and no role record, and refuses no config
/// dir. #12077: `opted_in` is the value the caller holds (a request, a
/// persisted record or this process's switch), never read here from the env,
/// so a daemon whose env lacks the switch cannot delete an opted-in record.
/// Every launch
/// path (`tm launch`/`tm connect`, `tm run`, the managed spawn, resume and
/// relaunch, the divert worker) reaches the sandbox only through here.
///
/// # Errors
///
/// None while enforcement is off; the `Result` keeps the callers unchanged.
///
/// Test: `a_default_launch_writes_no_sandbox_key_12077`,
/// `a_launch_with_no_config_dir_is_not_refused_12077`,
/// `the_shared_launch_step_runs_the_cleanup_and_never_refuses_12077`.
pub fn apply_launch_sandbox(
    role: SandboxRole,
    inputs: &SandboxInputs,
    config_dir: Option<&Path>,
    opted_in: bool,
) -> Result<(), SandboxSettingsError> {
    // #12077 slice 3: remove tm's 1.14.14/1.14.15 keys once; a failure is
    // logged and never refuses the launch. Runs whatever the opt-in switch.
    // #12077 slice 3b: `config_dir` is the user tier 1.14.15 wrote; the switch
    // decides only whether the role record is deleted.
    crate::core::claude_sandbox::clean_tm_sandbox_keys_on_launch(
        &inputs.workspace,
        config_dir,
        &inputs.home,
        // #12077: the caller's opt-in, not this process's env.
        opted_in,
    );
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
/// #12077: no launch path calls it — tm never writes the sandbox into a
/// Claude settings file; the opt-in carries it inline instead.
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
#[allow(dead_code)] // #12077: kept for its tests; no launch path writes the sandbox.
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
