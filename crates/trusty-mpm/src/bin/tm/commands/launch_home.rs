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
/// [`resolve_launch_home`].
/// Test: `sandboxed_launch_home_is_the_sandbox_home_not_the_process_home`.
pub(crate) fn launch_home() -> anyhow::Result<Option<PathBuf>> {
    resolve_launch_home(
        std::env::var_os("TRUSTY_SANDBOX").as_deref(),
        std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).as_deref(),
        dirs::home_dir(),
    )
}

/// Pure core of [`launch_home`], with the env passed in.
///
/// Why: tests must not mutate process-global env (common-pitfalls.md).
/// What: `sandbox` exactly `1` gives `<parent of data_override>/home`, the
/// layout `sandbox_daemon.sh` creates; an unset, relative or rootless override
/// is an error, never a fall back to `home` (Fail-Open Check). Otherwise `home`.
/// Test: `sandboxed_launch_home_is_the_sandbox_home_not_the_process_home`,
/// `sandboxed_launch_home_refuses_without_a_data_override`,
/// `unsandboxed_launch_home_is_the_process_home`.
pub(crate) fn resolve_launch_home(
    sandbox: Option<&std::ffi::OsStr>,
    data_override: Option<&std::ffi::OsStr>,
    home: Option<PathBuf>,
) -> anyhow::Result<Option<PathBuf>> {
    // #11077: the sandbox flag routes every launch-time write into the sandbox.
    if sandbox != Some(std::ffi::OsStr::new("1")) {
        return Ok(home);
    }
    let data = data_override
        .filter(|raw| !raw.is_empty())
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "TRUSTY_SANDBOX=1 but TRUSTY_DATA_DIR_OVERRIDE is not an absolute path; \
                 refusing to write launch files under the real home (#11077)"
            )
        })?;
    let root = data
        .parent()
        .filter(|parent| parent.parent().is_some())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "TRUSTY_DATA_DIR_OVERRIDE={} has no sandbox dir above it; refusing to \
             write launch files under the real home (#11077)",
                data.display()
            )
        })?;
    Ok(Some(root.join("home")))
}

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
    let root = home.map(|h| trusty_mpm::core::paths::FrameworkPaths::under(h).crate_config_root());
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
}
