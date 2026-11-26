//! The user-home writes `tm launch` / `tm connect` perform, under a named home.
//!
//! Why (#8545): both steps resolved the process home themselves, so the
//! `guided_fallback_*` tests that drive the real launch path provisioned the
//! operator's own `~/.trusty-tools/trusty-mpm/claude-config` and swept their
//! `~/.claude` settings. Taking `home` lets a test name a temp dir; production
//! passes `dirs::home_dir()`. Split out of `launch.rs`, which sits at the SLOC cap.
//! Test: `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`.

use std::path::{Path, PathBuf};

/// The home `tm launch` / `tm connect` write their launch files under.
///
/// Why (#11077): a sandboxed client (`TRUSTY_SANDBOX=1`, the env
/// `scripts/sandbox_daemon.sh` sets up) wrote `session-mcp/` and its owner
/// settings under the client's own `$HOME`, outside the sandbox.
/// What: reads the env and the process home, then defers to
/// [`resolve_launch_home`], which lives in the library.
/// Test: `sandboxed_launch_home_is_the_sandbox_home_not_the_process_home`.
pub(crate) fn launch_home() -> anyhow::Result<Option<PathBuf>> {
    resolve_launch_home(
        std::env::var_os("TRUSTY_SANDBOX").as_deref(),
        std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).as_deref(),
        dirs::home_dir(),
    )
    .map_err(|err| anyhow::anyhow!(err))
}

// #10102: the pure core moved to the library, so the daemon's managed spawn
// names the same sandbox-aware home.
pub(crate) use trusty_mpm::core::session_launch::resolve_launch_home;

/// Relocate `CLAUDE_CONFIG_DIR` for an interactive launch, provisioning and
/// trust-seeding it under `home`.
///
/// Why (issue #4181): one call shared by `tm launch` and `tm connect`. Why
/// relocation preserves #1269 lives on the library function.
/// What: delegates to
/// [`trusty_mpm::core::managed_config::prepare_interactive_config_dir_under`], then
/// warns once when the spawn will relocate with no resolvable
/// `CLAUDE_CODE_OAUTH_TOKEN` — the #2246 shape where a Keychain credential
/// stored under the operator's default config dir is unreadable under the
/// relocated one.
/// Test: `core::managed_config`'s `interactive_config_dir_*` tests.
pub(crate) fn relocate_config_dir(workspace: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let dir =
        trusty_mpm::core::managed_config::prepare_interactive_config_dir_under(workspace, home);
    // #2246: relocating moves which Keychain entry `claude` reads. Say so once
    // rather than let the session present as "logged in, then not logged in".
    if dir.is_some() && trusty_mpm::core::oauth_token::resolve_oauth_token().is_none() {
        eprintln!(
            "warning: no CLAUDE_CODE_OAUTH_TOKEN resolved; this session reads the \
             keychain entry keyed to the tm-managed config dir. If it starts \
             unauthenticated, run `claude setup-token | tm auth set-token` \
             (see issue #2246)."
        );
    }
    dir
}

/// Write and verify the launch's sandbox in `workspace` (#10102).
///
/// Why: #8928 step 6 — `tm launch`, `tm connect` and the Architect launch
/// start `claude` only inside the sandbox their role asked for.
/// What: [`trusty_mpm::core::session_launch::apply_launch_sandbox`] for
/// `profile`'s role, with `config_dir` (the relocated `CLAUDE_CONFIG_DIR`) as
/// the user tier. FAIL-CLOSED: no home, no config dir, or a read-back that
/// does not verify is an error, and every caller refuses its launch on it.
/// Test: `apply_sandbox_refuses_without_a_config_dir_and_applies_with_one`,
/// `apply_sandbox_refuses_a_workspace_that_does_not_verify`.
pub(crate) fn apply_sandbox(
    workspace: &Path,
    home: Option<&Path>,
    config_dir: Option<&Path>,
    profile: trusty_mpm::core::session_profile::SessionProfile,
) -> anyhow::Result<()> {
    use trusty_mpm::core::session_launch as sl;
    let home = home.ok_or_else(|| {
        anyhow::anyhow!("no home directory resolved; the sandbox cannot be applied")
    })?;
    let inputs = sl::launch_sandbox_inputs(workspace, home);
    sl::apply_launch_sandbox(sl::sandbox_role_for(profile), &inputs, config_dir)
        .map_err(|err| anyhow::anyhow!("{err}"))
}

/// Compose the session-scoped MCP config and the owner settings file under
/// `home`'s `~/.trusty-tools/trusty-mpm/` (#7422, #8545, #8988).
///
/// Why: `provision_for_spawn` resolves that root from the process home, so the
/// `guided_fallback_*` tests still wrote the operator's `session-mcp/` after
/// round 2 gave `launch`/`connect` a `home`. #8988: the owner `--settings`
/// file belongs to the same launch, so it is provisioned under the same root.
/// What: `provision_for_spawn_at` under `FrameworkPaths::under(home)`; with no
/// home it keeps `provision_for_spawn`, which reports the unresolvable home.
/// Then [`trusty_mpm::core::launch_policy::provision_settings`] under the same
/// root, which fails the launch only on a broken `install.toml` (#11821
/// df-D1w); `profile` is the launch's resolved
/// profile, which decides whether the #8992 skill and tool parts apply.
/// Test: `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`,
/// `launch_files_add_no_settings_without_an_owner`; the `--settings` token
/// itself: `isolated_spec_carries_the_owner_settings_after_the_mcp_config`.
pub(crate) fn launch_files(
    workspace: &Path,
    config_dir: Option<&Path>,
    home: Option<&Path>,
    profile: trusty_mpm::core::session_profile::SessionProfile,
) -> anyhow::Result<trusty_mpm::core::launch_policy::LaunchFiles> {
    use trusty_mpm::core::launch_policy::{provision_settings, provision_settings_default};
    use trusty_mpm::core::session_mcp_scope::{provision_for_spawn, provision_for_spawn_at};
    // #11821 P3: the layout of `home`'s install.
    let root =
        home.map(|h| trusty_mpm::core::paths::FrameworkPaths::under_home(h).crate_config_root());
    let provisioned = match &root {
        Some(root) => provision_for_spawn_at(root, workspace, config_dir),
        None => provision_for_spawn(workspace, config_dir),
    };
    let mcp_config = provisioned
        .map_err(|err| anyhow::anyhow!("failed to compose the session-scoped MCP config: {err}"))?;
    // #8988: the owner MCP policy; a failure is a printed notice, never a refusal.
    // #8992: plus the skill and tool parts for a PM `profile`.
    // #11821: except a broken install.toml, which refuses the launch (df-D1w).
    let settings = match &root {
        Some(root) => provision_settings(root, workspace, config_dir, profile),
        None => provision_settings_default(workspace, config_dir, profile),
    }?;
    Ok(trusty_mpm::core::launch_policy::LaunchFiles {
        mcp_config,
        settings: settings.file,
    })
}

/// Strip trusty-mpm hooks from `home`'s two `~/.claude/settings*.json` files.
///
/// #5875: those two files only, never a walk of the home tree. Best-effort: a
/// failure warns and the launch continues; no home resolved means nothing to strip.
pub(crate) fn strip_global_hooks(home: Option<&Path>) {
    let Some(home) = home else { return };
    if let Err(e) = trusty_mpm::core::standalone::hooks::remove_global_trusty_mpm_hooks_at(home) {
        eprintln!("warning: could not remove global MPM hooks: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::apply_sandbox;

    /// #8988: a workspace with no owner gets its session-MCP file and no
    /// settings file, so `tm launch`/`tm connect` argv stays as before.
    #[test]
    fn launch_files_add_no_settings_without_an_owner() {
        let home = crate::test_support::hermetic_temp_dir();
        let workspace = crate::test_support::hermetic_temp_dir();
        let config = home.path().join("cfg");
        let pm = trusty_mpm::core::session_profile::SessionProfile::Pm;
        let files = super::launch_files(workspace.path(), Some(&config), Some(home.path()), pm)
            .expect("the session-MCP file is writable under a temp home");
        assert!(files.mcp_config.is_some());
        assert_eq!(files.settings, None);
    }
    /// #11077: a sandboxed client resolves `<sandbox>/home`, never `$HOME`.
    #[test]
    fn sandboxed_launch_home_is_the_sandbox_home_not_the_process_home() {
        use std::ffi::OsStr;
        use std::path::PathBuf;
        let got = super::resolve_launch_home(
            Some(OsStr::new("1")),
            Some(OsStr::new("/tmp/sbx/data")),
            Some(PathBuf::from("/Users/example")),
        )
        .expect("a sandbox with a data override resolves");
        assert_eq!(got, Some(PathBuf::from("/tmp/sbx/home")));
    }

    /// #11077 Fail-Open Check: no usable override under the flag is a refusal.
    #[test]
    fn sandboxed_launch_home_refuses_without_a_data_override() {
        use std::ffi::OsStr;
        use std::path::PathBuf;
        let home = Some(PathBuf::from("/Users/example"));
        for data in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("rel/data")),
            Some(OsStr::new("/data")),
        ] {
            let got = super::resolve_launch_home(Some(OsStr::new("1")), data, home.clone());
            assert!(got.is_err(), "{data:?} must refuse, got {got:?}");
        }
    }

    /// #11077: with no sandbox flag the process home is kept unchanged.
    #[test]
    fn unsandboxed_launch_home_is_the_process_home() {
        use std::ffi::OsStr;
        use std::path::PathBuf;
        let home = Some(PathBuf::from("/Users/example"));
        for flag in [None, Some(OsStr::new("0"))] {
            let got =
                super::resolve_launch_home(flag, Some(OsStr::new("/tmp/sbx/data")), home.clone());
            assert_eq!(got.expect("no sandbox never refuses"), home);
        }
    }

    /// #10102: no config dir refuses; a tm-owned one applies and verifies.
    #[test]
    fn apply_sandbox_refuses_without_a_config_dir_and_applies_with_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).expect("mkdir");
        let home = root.join("home");
        let pm = trusty_mpm::core::session_profile::SessionProfile::Pm;
        let err = apply_sandbox(&ws, Some(&home), None, pm).expect_err("unset refuses");
        assert!(err.to_string().contains("CLAUDE_CONFIG_DIR"), "{err}");
        assert!(!home.join(".claude").exists(), "no `~/.claude` write");
        let config = home.join(".trusty-tools/trusty-mpm/claude-config");
        apply_sandbox(&ws, Some(&home), Some(&config), pm).expect("applies");
        assert!(config.join("settings.json").exists());
    }

    /// #10102 Fail-Open Check: a workspace whose read-back fails is an error.
    #[test]
    fn apply_sandbox_refuses_a_workspace_that_does_not_verify() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join(".claude")).expect("mkdir");
        std::fs::write(
            ws.join(".claude/settings.local.json"),
            r#"{"sandbox":{"enabled":false}}"#,
        )
        .expect("plant");
        let home = root.join("home");
        let config = home.join(".trusty-tools/trusty-mpm/claude-config");
        let pm = trusty_mpm::core::session_profile::SessionProfile::Pm;
        let err = apply_sandbox(&ws, Some(&home), Some(&config), pm).expect_err("refuses");
        assert!(err.to_string().contains("settings.local.json"), "{err}");
    }
}
