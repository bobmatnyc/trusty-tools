//! The user-home writes `tm launch` / `tm connect` perform, under a named home.
//!
//! Why (#8545): both steps resolved the process home themselves, so the
//! `guided_fallback_*` tests that drive the real launch path provisioned the
//! operator's own `~/.trusty-tools/trusty-mpm/claude-config` and swept their
//! `~/.claude` settings. Taking `home` lets a test name a temp dir; production
//! passes `dirs::home_dir()`. Split out of `launch.rs`, which sits at the SLOC cap.
//! Test: `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`.

use std::path::{Path, PathBuf};

/// Relocate `CLAUDE_CONFIG_DIR` for an interactive launch, provisioning and
/// trust-seeding it under `home`.
///
/// Why (issue #4181): one call shared by `tm launch` and `tm connect`. Why
/// relocation preserves #1269 lives on the library function.
/// What: delegates to
/// [`trusty_mpm::core::managed_config::prepare_interactive_config_dir`], then
/// warns once when the spawn will relocate with no resolvable
/// `CLAUDE_CODE_OAUTH_TOKEN` — the #2246 shape where a Keychain credential
/// stored under the operator's default config dir is unreadable under the
/// relocated one.
/// Test: `core::managed_config`'s `interactive_config_dir_*` tests.
pub(crate) fn relocate_config_dir(workspace: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let dir = trusty_mpm::core::managed_config::prepare_interactive_config_dir(workspace, home);
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

/// Compose the session-scoped MCP config under `home`'s
/// `~/.trusty-tools/trusty-mpm/session-mcp/` (#7422, #8545).
///
/// Why: `provision_for_spawn` resolves that root from the process home, so the
/// `guided_fallback_*` tests still wrote the operator's `session-mcp/` after
/// round 2 gave `launch`/`connect` a `home`.
/// What: `provision_for_spawn_at` under `FrameworkPaths::under(home)`; with no
/// home it keeps `provision_for_spawn`, which reports the unresolvable home.
/// Test: `guided_fallback_prepares_the_session_in_the_worktree_not_the_base_clone`.
pub(crate) fn provision_session_mcp(
    workspace: &Path,
    config_dir: Option<&Path>,
    home: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    use trusty_mpm::core::session_mcp_scope::{provision_for_spawn, provision_for_spawn_at};
    let provisioned = match home {
        Some(home) => {
            let root = trusty_mpm::core::paths::FrameworkPaths::under(home).crate_config_root();
            provision_for_spawn_at(&root, workspace, config_dir)
        }
        None => provision_for_spawn(workspace, config_dir),
    };
    provisioned
        .map_err(|err| anyhow::anyhow!("failed to compose the session-scoped MCP config: {err}"))
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
