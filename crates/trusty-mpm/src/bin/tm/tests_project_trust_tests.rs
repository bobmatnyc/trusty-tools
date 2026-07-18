//! `tm project trust` / `--revoke` coverage (issue #3033 security fix).
//!
//! Why: split into its own file (rather than growing `tests.rs` or
//! `tests_behavior_a.rs`, both already near their SLOC caps) mirroring the
//! established `tests_behavior_*_tests.rs` split convention — files ending in
//! `_tests.rs` get the 1500-SLOC test-file cap.
//! What: CLI parse round-trips for `ProjectAction::Trust`, plus a behavioral
//! test driving `commands::project::trust_cmd` (the local, daemon-free
//! grant/revoke handler) against a faked `$HOME` so it never touches the
//! operator's real trust store.
//! Test: this file IS the test module.

use clap::Parser;

use crate::cli::{Cli, Command, ProjectAction};
use crate::commands::project::trust_cmd;

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

/// Point `$HOME` at `home` for the body, restoring it afterwards even on
/// panic — mirrors the equivalent guard in `custom_mcp_tests.rs`.
fn with_fake_home<F: FnOnce()>(home: &std::path::Path, body: F) {
    let prev = std::env::var("HOME").ok();
    // SAFETY: callers are `#[serial_test::serial]`, so no other thread races
    // this set/restore; the restore runs regardless of a panic in `body`.
    unsafe { std::env::set_var("HOME", home) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    match prev {
        Some(p) => unsafe { std::env::set_var("HOME", p) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

#[test]
#[serial_test::serial]
fn project_trust_grants_and_revokes() {
    // Why: `trust_cmd` is the only way to flip `core::project_trust`'s
    // user-scope state; this proves the full grant -> query -> revoke cycle
    // through the actual CLI handler, isolated to a faked `$HOME`.
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_dir = project.path().to_string_lossy().to_string();

    with_fake_home(home.path(), || {
        assert!(!trusty_mpm::core::project_trust::is_project_trusted(
            project.path()
        ));

        trust_cmd(Some(project_dir.clone()), false).expect("trust succeeds");
        assert!(trusty_mpm::core::project_trust::is_project_trusted(
            project.path()
        ));

        // Trust state must live under the FAKE home, never inside the
        // project directory itself (a cloned repo must not be able to
        // self-trust).
        assert!(
            !project.path().join("project-trust.json").exists(),
            "trust state must never be written inside the project directory"
        );
        let store_path = home
            .path()
            .join(".trusty-tools")
            .join("trusty-mpm")
            .join("project-trust.json");
        assert!(
            store_path.exists(),
            "trust state must live under the user-scope tm config dir"
        );

        trust_cmd(Some(project_dir), true).expect("revoke succeeds");
        assert!(!trusty_mpm::core::project_trust::is_project_trusted(
            project.path()
        ));
    });
}
