//! Tests for the launch-spec carrier (#8233).
//!
//! Why: the spec is what a managed launch's parameters travel in, and it carries
//! credentials, so both halves need proving — the round trip (what the shim
//! reads is what the daemon wrote) and the permissions (0600 inside 0700,
//! created with the mode, removed on consume).
//! What: hermetic — every test writes into its own `TempDir`; nothing touches
//! the operator's real config home.
//! Test: this file.

use std::path::{Path, PathBuf};

use super::{LaunchSpec, LaunchSpecError};

/// A representative spec with every field populated.
fn sample() -> LaunchSpec {
    LaunchSpec {
        session_id: "11111111-2222-3333-4444-555555555555".to_owned(),
        cwd: PathBuf::from("/workspace/repo"),
        program: "/abs/claude".to_owned(),
        args: vec!["--resume".to_owned(), "abc".to_owned()],
        env_unset: vec!["ANTHROPIC_API_KEY".to_owned(), "GH_TOKEN".to_owned()],
        env_set: vec![
            ("CLAUDE_CONFIG_DIR".to_owned(), "/managed/cfg".to_owned()),
            ("GH_TOKEN".to_owned(), "gho_secret".to_owned()),
        ],
    }
}

/// The unix mode bits of `path`, or `None` off unix.
#[cfg(unix)]
fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[test]
fn spec_root_nests_under_the_tm_config_home() {
    // The shim and the daemon must look in the same place. A `None` here means
    // home is unresolvable, which is its own error path.
    if let Some(root) = LaunchSpec::root() {
        assert!(
            root.ends_with("launch-specs"),
            "spec root must be the launch-specs directory: {}",
            root.display()
        );
        assert!(
            root.to_string_lossy().contains("trusty-mpm"),
            "spec root must live under the tm config home: {}",
            root.display()
        );
    }
}

#[test]
fn spec_round_trips_through_the_file() {
    // What the shim reads must be exactly what the daemon wrote — this is the
    // whole contract the typed line no longer carries.
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = sample();
    let path = spec.write_in(dir.path()).expect("write");
    let read = LaunchSpec::consume(&path).expect("consume");
    assert_eq!(read, spec);
}

#[cfg(unix)]
#[test]
fn write_creates_an_owner_only_file_in_an_owner_only_dir() {
    // #8233: the spec carries GH_TOKEN and CLAUDE_CODE_OAUTH_TOKEN. It is
    // created WITH mode 0600 (never chmod'd after, so never world-readable for
    // an instant) inside a 0700 directory.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("launch-specs");
    let path = sample().write_in(&root).expect("write");
    assert_eq!(mode_of(&path), Some(0o600), "spec file must be owner-only");
    assert_eq!(mode_of(&root), Some(0o700), "spec dir must be owner-only");
}

#[test]
fn consume_removes_the_file() {
    // Single-use: a spec left behind is a credential on disk and a launch that
    // could be replayed into a second `claude`.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = sample().write_in(dir.path()).expect("write");
    assert!(path.exists());
    let _ = LaunchSpec::consume(&path).expect("consume");
    assert!(!path.exists(), "consume must delete the spec it read");
}

#[test]
fn consume_reports_a_missing_spec() {
    // Fail-closed: the shim must say so and launch nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let err = LaunchSpec::consume(&dir.path().join("absent.json"))
        .expect_err("a missing spec must error");
    assert!(matches!(err, LaunchSpecError::Read { .. }), "got {err:?}");
}

#[test]
fn consume_reports_a_corrupt_spec() {
    // A truncated or clobbered spec must not launch a partially-configured
    // `claude` — the exact failure mode a truncated command line produced.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("corrupt.json");
    std::fs::write(&path, b"{\"session_id\": ").expect("seed");
    let err = LaunchSpec::consume(&path).expect_err("a corrupt spec must error");
    assert!(matches!(err, LaunchSpecError::Parse { .. }), "got {err:?}");
}

#[test]
fn verify_accepts_a_spec_just_written() {
    // The daemon's read-back check must not consume the spec the pane is about
    // to be told to read.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = sample().write_in(dir.path()).expect("write");
    assert_eq!(LaunchSpec::verify(&path).expect("verify"), sample());
    assert!(path.exists(), "verify must leave the spec in place");
}

#[test]
fn verify_reports_a_corrupt_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("corrupt.json");
    std::fs::write(&path, b"not json").expect("seed");
    let err = LaunchSpec::verify(&path).expect_err("a corrupt spec must error");
    assert!(matches!(err, LaunchSpecError::Parse { .. }), "got {err:?}");
}

#[cfg(unix)]
#[test]
fn write_reports_an_unwritable_directory() {
    // A spec that cannot be written must abort the launch, never fall back to
    // typing an unbounded line.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let blocked = dir.path().join("blocked");
    std::fs::create_dir(&blocked).expect("mkdir");
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let err = sample()
        .write_in(&blocked.join("nested"))
        .expect_err("an unwritable root must error");
    assert!(
        matches!(
            err,
            LaunchSpecError::Dir { .. } | LaunchSpecError::Write { .. }
        ),
        "got {err:?}"
    );
    // Restore so the TempDir can clean itself up.
    let _ = std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700));
}

#[test]
fn spec_command_carries_cwd_program_argv_and_env() {
    // #8233 requirement 3: the claude the OS sees must have the same cwd, env
    // and argv the shell line produced — asserted against the resolved
    // parameters, not against a string.
    let spec = sample();
    let cmd = spec.to_command();
    assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/abs/claude"));
    let argv: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv, vec!["--resume".to_owned(), "abc".to_owned()]);
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/workspace/repo")));

    let envs: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert!(
        envs.contains(&("ANTHROPIC_API_KEY".to_owned(), None)),
        "the API key must be REMOVED, not assigned: {envs:?}"
    );
    assert!(
        envs.contains(&(
            "CLAUDE_CONFIG_DIR".to_owned(),
            Some("/managed/cfg".to_owned())
        )),
        "the managed config dir must be assigned: {envs:?}"
    );
    assert!(
        envs.contains(&("GH_TOKEN".to_owned(), Some("gho_secret".to_owned()))),
        "a later assignment must win over the earlier unset of the same name: {envs:?}"
    );
    assert!(
        envs.contains(&(
            crate::core::harness_root::MANAGED_SESSION_ID_ENV.to_owned(),
            Some(spec.session_id.clone())
        )),
        "#2023 B: the managed session id must reach the child: {envs:?}"
    );
}

#[test]
fn spec_command_yields_the_alt_screen_default_to_the_pane() {
    // #6495/#7160: the `${NAME-1}` shell operand became
    // `alt_screen::apply_default_to_command`. Whichever way the ambient
    // environment answers, exactly one outcome is correct: the variable is
    // assigned tm's default when the pane does not already carry it, and is left
    // to the pane's own value when it does.
    let cmd = sample().to_command();
    let assigned: Option<String> = cmd
        .get_envs()
        .find(|(k, _)| k.to_string_lossy() == crate::core::alt_screen::ALT_SCREEN_ENV_VAR)
        .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()));
    let inherited = std::env::var_os(crate::core::alt_screen::ALT_SCREEN_ENV_VAR).is_some();
    if inherited {
        assert!(
            assigned.is_none(),
            "an operator value in the pane must win, not be overridden: {assigned:?}"
        );
    } else {
        assert_eq!(
            assigned.as_deref(),
            Some(crate::core::alt_screen::ALT_SCREEN_DEFAULT),
            "with nothing inherited the tm default must be provisioned"
        );
    }
}
