//! Coverage for the per-project MCP environment (#4181, ADR-0042).
//!
//! Why: this module is what replaces the two per-project pins the deleted
//! injectors wrote into a workspace `.mcp.json` — `env.TRUSTY_MEMORY_PALACE`
//! (#1605) and the `serve --index <id>` argument (#1373). If the toggle gating
//! or the derivation regresses, both of those bugs return with no other test
//! failing, because nothing else asserts a per-project pin any more. The
//! manifest-toggle half is the successor subject of
//! `tests_manifest_toggle_trust_3934.rs`, which pinned the same two toggles
//! against the trust derivation that ADR-0042 deleted.
//! What: `resolve_conditional_mcp_toggles` default and project-manifest cases
//! (moved verbatim with the function from `mcp_config_tests.rs`), plus the
//! toggle → export gating in `session_mcp_env_with`.
//! Test: this is the test module.

use super::*;
use tempfile::tempdir;

/// Write a project-scope manifest at
/// `<project>/.trusty-mpm/framework/manifest.toml` — the git-tracked file that
/// travels with a cloned repo, and the layer `ManifestSources::resolve` reads
/// at highest precedence.
fn write_manifest(project: &std::path::Path, body: &str) {
    let dir = project.join(".trusty-mpm").join("framework");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.toml"), body).unwrap();
}

#[test]
fn resolve_conditional_mcp_toggles_defaults_to_both_on() {
    let tmp = tempdir().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp.path());
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();

    assert_eq!(resolve_conditional_mcp_toggles(&fw, &project), (true, true));
}

#[test]
fn resolve_conditional_mcp_toggles_honors_project_manifest_toggle() {
    let tmp = tempdir().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp.path());
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_manifest(&project, "[mcp]\ntrusty_memory = false\n");

    assert_eq!(
        resolve_conditional_mcp_toggles(&fw, &project),
        (false, true)
    );
}

#[test]
#[serial_test::serial]
fn session_mcp_env_omits_both_when_both_toggles_are_off() {
    // Both toggles off must export nothing at all — a session that opted out of
    // the integrations must not be handed a palace or an index anyway, which is
    // the gating half `tests_manifest_toggle_trust_3934.rs` used to assert
    // against the (now deleted) trust derivation.
    let tmp = tempdir().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp.path());
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_manifest(
        &project,
        "[mcp]\ntrusty_memory = false\ntrusty_search = false\n",
    );

    let env = session_mcp_env_with(&fw, &project, Some("git@github.com:acme/widget.git"));
    assert!(env.is_empty(), "both toggles off exports nothing: {env:?}");
}

#[test]
#[serial_test::serial]
fn session_mcp_env_omits_palace_when_memory_disabled() {
    let tmp = tempdir().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp.path());
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_manifest(
        &project,
        "[mcp]\ntrusty_memory = false\ntrusty_search = false\n",
    );

    let env = session_mcp_env_with(&fw, &project, Some("git@github.com:acme/widget.git"));
    assert!(
        !env.iter()
            .any(|(name, _)| name == trusty_common::PALACE_OVERRIDE_ENV),
        "a disabled trusty-memory toggle must not export a palace pin: {env:?}"
    );
}

#[test]
#[serial_test::serial]
fn session_mcp_env_exports_palace_when_memory_enabled() {
    // The #1605 pin, now carried by the environment instead of the `.mcp.json`
    // `env` block the deleted injector wrote. The slug must be the repo's
    // canonical `owner-repo` identity, not the throwaway workspace basename.
    // SAFETY: `#[serial_test::serial]` keeps this the only thread mutating the
    // process environment for the duration of this test.
    unsafe { std::env::remove_var(trusty_common::PALACE_OVERRIDE_ENV) };
    let tmp = tempdir().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp.path());
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    // Search off so this test never touches the trusty-search daemon.
    write_manifest(&project, "[mcp]\ntrusty_search = false\n");

    let env = session_mcp_env_with(&fw, &project, Some("git@github.com:acme/widget.git"));
    let palace = env
        .iter()
        .find(|(name, _)| name == trusty_common::PALACE_OVERRIDE_ENV)
        .map(|(_, value)| value.clone());
    assert_eq!(
        palace.as_deref(),
        Some("acme-widget"),
        "the exported palace must be the repo identity, not the workspace basename: {env:?}"
    );
}

#[test]
#[serial_test::serial]
fn session_mcp_env_omits_index_when_search_disabled() {
    // The gate that keeps a disabled `[mcp] trusty_search` from both touching
    // the daemon and pinning an index (#1373's carrier, now an env var).
    let tmp = tempdir().unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp.path());
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_manifest(&project, "[mcp]\ntrusty_search = false\n");

    let env = session_mcp_env_with(&fw, &project, None);
    assert!(
        !env.iter().any(|(name, _)| name == SEARCH_INDEX_ENV),
        "a disabled trusty-search toggle must not export an index pin: {env:?}"
    );
}
