//! `tm project trust` / `--revoke` coverage (issue #3033 security fix).
//!
//! Why: split into its own file (rather than growing `tests.rs` or
//! `tests_behavior_a.rs`, both already near their SLOC caps) mirroring the
//! established `tests_behavior_*_tests.rs` split convention — files ending in
//! `_tests.rs` get the 1500-SLOC test-file cap.
//! What: CLI parse round-trips for `ProjectAction::Trust`, plus a behavioral
//! test driving `commands::project::trust_cmd` (the local, daemon-free
//! grant/revoke handler) against an injected store root so it never touches
//! the operator's real trust store — and never repoints the process's `$HOME`
//! to get there (#5544).
//! Test: this file IS the test module.

use clap::Parser;

use crate::cli::{Cli, Command, ProjectAction};
use crate::commands::project::trust_cmd_in;

#[test]
fn cli_parses_project_trust() {
    // Why (issue #3033): `tm project trust` is the consent gate for
    // project-scope custom MCP bridging — must parse with an optional `--dir`
    // and default `revoke` to `false`.
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "trust", "--dir", "/work/p"]).unwrap();
    match cli.command.unwrap() {
        Command::Project {
            action: ProjectAction::Trust { dir, revoke },
        } => {
            assert_eq!(dir.as_deref(), Some("/work/p"));
            assert!(!revoke);
        }
        other => panic!("expected project trust, got {other:?}"),
    }
}

#[test]
fn cli_parses_project_trust_revoke() {
    let cli = Cli::try_parse_from(["trusty-mpm", "project", "trust", "--revoke"]).unwrap();
    match cli.command.unwrap() {
        Command::Project {
            action: ProjectAction::Trust { dir, revoke },
        } => {
            assert_eq!(dir, None);
            assert!(revoke);
        }
        other => panic!("expected project trust --revoke, got {other:?}"),
    }
}

#[test]
fn project_trust_grants_and_revokes() {
    // Why: `trust_cmd_in` is the only way to flip `core::project_trust`'s
    // user-scope state; this proves the full grant -> query -> revoke cycle
    // through the actual CLI handler.
    //
    // #5544: the store ROOT is injected, not reached via a repointed `$HOME`.
    // The previous revision pointed the process's `$HOME` at a tempdir behind
    // `#[serial]`. `cargo test` runs a target's tests as threads in ONE
    // process, so that write was visible to every sibling for its duration,
    // and `#[serial]` excludes only other `#[serial]` tests. `$HOME` is read
    // transitively — `dirs::home_dir`, `FrameworkPaths`, and the three-tier
    // agent-roster scan all consult it — so the set of readers that could
    // straddle the repoint was unbounded. `trust_cmd_in` removes the write;
    // `#[serial]` is gone with it.
    let store_root = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_dir = project.path().to_string_lossy().to_string();
    let root = store_root.path();

    assert!(!trusty_mpm::core::project_trust::is_project_trusted_at(
        project.path(),
        root
    ));

    trust_cmd_in(Some(project_dir.clone()), false, root).expect("trust succeeds");
    assert!(trusty_mpm::core::project_trust::is_project_trusted_at(
        project.path(),
        root
    ));

    // Trust state must live under the injected store root, never inside the
    // project directory itself (a cloned repo must not be able to self-trust).
    assert!(
        !project.path().join("project-trust.json").exists(),
        "trust state must never be written inside the project directory"
    );
    assert!(
        root.join("project-trust.json").exists(),
        "trust state must live under the user-scope tm config dir"
    );

    trust_cmd_in(Some(project_dir), true, root).expect("revoke succeeds");
    assert!(!trusty_mpm::core::project_trust::is_project_trusted_at(
        project.path(),
        root
    ));
}

/// The production root the injected one stands in for.
///
/// Why (#5544): `trust_cmd_in` takes the root, so nothing else asserts WHERE
/// production puts the store. Reading `$HOME` is safe — only writing it is the
/// hazard — so this pins the layout without reintroducing the mutation.
/// What: asserts `trust_store_root()` resolves under the user-scope tm config
/// directory rather than anywhere project-relative.
/// Test: this function IS the test.
#[test]
fn trust_store_root_is_the_user_scope_tm_config_dir() {
    let root = trusty_mpm::core::project_trust::trust_store_root()
        .expect("a home directory resolves on every supported platform");
    assert!(
        root.ends_with(std::path::Path::new(".trusty-tools").join("trusty-mpm")),
        "the trust store must live under the user-scope tm config dir, got {}",
        root.display()
    );
}
