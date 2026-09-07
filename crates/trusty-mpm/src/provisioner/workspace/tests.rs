//! Unit tests for `provisioner::workspace`.
//!
//! Why: extracted from `workspace.rs` so the production module keeps its
//! Why/What/Test documentation density under the mechanical SLOC gate
//! (`scripts/check_line_cap.sh`), whose test/benchmark cap this file's
//! `tests.rs` basename selects. ADR-0055 (#6000) removed the session-workspace
//! provisioner most of this file covered; what remains is the per-project
//! identity binding `content::catalog_sync` depends on.
//! What: #2184 — the resolved identity is applied to every command the backend
//! builds, and an empty identity leaves the command untouched.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;

// ── #2184: RealGitBackend applies the resolved identity to every command ──

/// Why: a `RealGitBackend::default()` (no identity resolved) must build a
/// PLAIN `git` command — no env overrides, no `-c` args — so every existing
/// production call site (which constructs `RealGitBackend::default()` when it
/// has no project context) is byte-for-byte unaffected by #2184.
/// Test: itself.
#[test]
fn default_identity_produces_plain_git_command() {
    let backend = RealGitBackend::default();
    let cmd = backend.command();
    assert_eq!(
        cmd.get_args().count(),
        0,
        "no -c args for an empty identity"
    );
    assert_eq!(
        cmd.get_envs().count(),
        0,
        "no env overrides for an empty identity"
    );
}

/// Why: the resolved `GitIdentity::env` overrides must be applied to every
/// command this backend builds, so `git`/its credential helper authenticate
/// as the right per-project identity.
/// Test: itself.
#[test]
fn git_identity_env_applied_to_command() {
    let identity = crate::core::git_identity::GitIdentity {
        env: vec![("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())],
        env_remove: vec!["GH_TOKEN".to_string()],
        commit_name: None,
        commit_email: None,
    };
    let backend = RealGitBackend::new(identity);
    let cmd = backend.command();
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(
        envs.iter().any(|(k, v)| {
            *k == std::ffi::OsStr::new("GH_CONFIG_DIR")
                && *v == Some(std::ffi::OsStr::new("/cfg/project"))
        }),
        "GH_CONFIG_DIR override must be applied: {envs:?}"
    );
    // #6668: a credential helper reads GH_TOKEN ahead of GH_CONFIG_DIR, so the
    // removal has to reach the command too or the binding stays decorative.
    assert!(
        envs.iter()
            .any(|(k, v)| *k == std::ffi::OsStr::new("GH_TOKEN") && v.is_none()),
        "GH_TOKEN must be removed: {envs:?}"
    );
}

/// Why: a resolved commit-identity override must render as `-c user.name=…`/
/// `-c user.email=…` BEFORE any subcommand arg (git only accepts `-c`
/// overrides in that position).
/// Test: itself.
#[test]
fn git_identity_commit_args_applied_to_command() {
    let identity = crate::core::git_identity::GitIdentity {
        env: vec![],
        env_remove: vec![],
        commit_name: Some("Bot".to_string()),
        commit_email: Some("bot@example.com".to_string()),
    };
    let backend = RealGitBackend::new(identity);
    let cmd = backend.command();
    let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
    assert_eq!(
        args,
        vec![
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new("user.name=Bot"),
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new("user.email=bot@example.com"),
        ]
    );
}
