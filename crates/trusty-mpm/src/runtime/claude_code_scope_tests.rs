//! Argv and fail-closed regression tests for default-deny MCP scoping (#7422).
//!
//! Why: the scoping is only real if two things hold, and BOTH are silent when
//! they break. The flags must reach the spawned process — a missing flag just
//! loads everything, exactly as before the issue. And a `provision` failure must
//! abandon the spawn — a path that swallowed the error would launch with no
//! `--mcp-config`, which is the unscoped shared map in a different disguise.
//! Every builder that relocates `CLAUDE_CONFIG_DIR` therefore gets its own pin
//! for each, so a future edit to one path cannot regress it while the others
//! keep passing.
//! What: `spawn_command`, `resume_command` and `compose_inplace_args` asserted
//! both ways on the flags; then `ClaudeCodeAdapter::spawn`, `spawn_resume` and
//! `build_inplace_resume_command` asserted to return `Err` — and to send no
//! command to the pane — when the composed file cannot be written.
//! Test: this file IS the test suite.

use super::*;

use crate::runtime::test_helpers::FakeTmux;

/// Representative managed-session UUID; not a real session.
const SCOPE_SESSION_ID: &str = "99999999-8888-7777-6666-555555555555";

/// Representative cwd; these builders are pure string composition.
const SCOPE_CWD: &str = "/tmp/scope-ws";

/// Representative relocated config dir.
const SCOPE_CONFIG_DIR: &str = "/tm/claude-config";

/// The `--mcp-config` path a session rooted at [`SCOPE_CWD`] must name.
///
/// Why: derived from `$HOME`, which [`PoisonedStateRoot`] rewrites from the
/// fail-closed tests further down THIS BINARY. An in-process `cargo test` runs
/// both sets on threads of one process, so a flag test reading `$HOME` while a
/// poisoned root is installed would compare against the tempdir and fail for a
/// reason that has nothing to do with the flags. Every test in this file is
/// therefore `#[serial_test::serial]` — the flag tests included, so the rule
/// holds for the whole file rather than for whichever tests happen to read
/// `$HOME` today (#7422).
/// What: `scoped_for` against the representative cwd and config dir.
fn expected_path() -> String {
    crate::core::session_mcp_scope::scoped_for(
        Path::new(SCOPE_CWD),
        Some(Path::new(SCOPE_CONFIG_DIR)),
    )
    .expect("HOME resolves in a test environment")
    .display()
    .to_string()
}

#[test]
#[serial_test::serial]
fn spawn_command_carries_the_strict_mcp_flags() {
    let cmd = spawn_command(
        Path::new(SCOPE_CWD),
        "claude",
        Some(Path::new(SCOPE_CONFIG_DIR)),
        SCOPE_SESSION_ID,
        None,
        None,
        None,
        &[],
    );
    assert!(
        cmd.contains("--strict-mcp-config"),
        "a relocated spawn must refuse every server outside its composed file: {cmd}"
    );
    assert!(
        cmd.contains(&format!("--mcp-config '{}'", expected_path())),
        "the flag must name this session's own composed file, single-quoted: {cmd}"
    );
}

#[test]
#[serial_test::serial]
fn spawn_command_omits_the_strict_mcp_flags_without_a_config_dir() {
    let cmd = spawn_command(
        Path::new(SCOPE_CWD),
        "claude",
        None,
        SCOPE_SESSION_ID,
        None,
        None,
        None,
        &[],
    );
    assert!(
        !cmd.contains("--strict-mcp-config"),
        "a spawn reading the operator's own ~/.claude.json is not tm's to scope: {cmd}"
    );
}

#[test]
#[serial_test::serial]
fn resume_command_carries_the_strict_mcp_flags() {
    let cmd = resume_command(
        Path::new(SCOPE_CWD),
        "claude",
        Some(Path::new(SCOPE_CONFIG_DIR)),
        Some("abc-123"),
        SCOPE_SESSION_ID,
        None,
        None,
        None,
        &[],
    );
    assert!(
        cmd.contains("--strict-mcp-config"),
        "a resumed pane must be scoped exactly like a fresh one: {cmd}"
    );
    assert!(
        cmd.contains(&format!("--mcp-config '{}'", expected_path())),
        "{cmd}"
    );
}

#[test]
#[serial_test::serial]
fn resume_command_omits_the_strict_mcp_flags_without_a_config_dir() {
    let cmd = resume_command(
        Path::new(SCOPE_CWD),
        "claude",
        None,
        Some("abc-123"),
        SCOPE_SESSION_ID,
        None,
        None,
        None,
        &[],
    );
    assert!(!cmd.contains("--strict-mcp-config"), "{cmd}");
}

#[test]
#[serial_test::serial]
fn compose_inplace_args_carries_the_strict_mcp_flags_unquoted() {
    let args = compose_inplace_args(
        Path::new(SCOPE_CWD),
        Some(Path::new(SCOPE_CONFIG_DIR)),
        None,
        None,
    );
    let pos = args
        .iter()
        .position(|a| a == "--mcp-config")
        .expect("the in-place relaunch must carry --mcp-config");
    assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    assert_eq!(
        args[pos + 1],
        expected_path(),
        "exec takes the token verbatim — a quoted path names a file claude cannot open"
    );
}

#[test]
#[serial_test::serial]
fn compose_inplace_args_omits_the_strict_mcp_flags_without_a_config_dir() {
    let args = compose_inplace_args(Path::new(SCOPE_CWD), None, None, None);
    assert!(!args.iter().any(|a| a == "--strict-mcp-config"), "{args:?}");
}

// ---------------------------------------------------------------------------
// Fail-closed arms: `provision` cannot write, so no command may be built.
// ---------------------------------------------------------------------------

/// A `$HOME` whose `session-mcp` state path is a regular FILE.
///
/// Why: every fail-closed test below needs `provision` to fail for a reason
/// that is deterministic on any machine and needs no permission games. A file
/// where `create_dir_all` must make a directory is exactly that.
/// What: redirects `$HOME` to a fresh tempdir, plants
/// `<home>/.trusty-tools/trusty-mpm/session-mcp` as a file, and restores the
/// prior `$HOME` on drop. Callers MUST be `#[serial_test::serial]`.
/// Test: used by every `…_fails_when_the_composed_file_cannot_be_written` test.
struct PoisonedStateRoot {
    prev: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
}

impl PoisonedStateRoot {
    fn set() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        // SAFETY: every caller is `#[serial]`, so no other test thread races
        // this set/restore; Drop restores it even on an unwinding panic.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let root = crate::core::session_mcp_scope::session_mcp_path(Path::new("/tmp"))
            .expect("HOME was just set");
        let dir = root.parent().expect("the composed file has a parent");
        std::fs::create_dir_all(dir.parent().expect("…which has a parent")).unwrap();
        std::fs::write(dir, "not a directory").unwrap();
        Self { prev, _tmp: tmp }
    }
}

impl Drop for PoisonedStateRoot {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Assert a `RuntimeError` is the fail-closed arm, not something incidental.
fn assert_scope_failure(err: &RuntimeError) {
    let text = err.to_string();
    assert!(
        matches!(err, RuntimeError::Spawn(_)),
        "a composition failure must abandon the spawn, got {err:?}"
    );
    assert!(
        text.contains("refusing to launch"),
        "the error must say why it did not fall back to the shared map: {text}"
    );
}

#[test]
#[serial_test::serial]
fn spawn_fails_when_the_composed_file_cannot_be_written() {
    let _home = PoisonedStateRoot::set();
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());

    let err = adapter
        .spawn(
            "tmpm-scope",
            Path::new("/tmp"),
            "task",
            SCOPE_SESSION_ID,
            &[],
        )
        .expect_err("an unwritable state root must abandon the spawn");

    assert_scope_failure(&err);
    assert!(
        fake.sends.lock().unwrap().is_empty(),
        "no command may reach the pane once composition failed"
    );
}

#[test]
#[serial_test::serial]
fn spawn_resume_fails_when_the_composed_file_cannot_be_written() {
    let _home = PoisonedStateRoot::set();
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone());

    let err = adapter
        .spawn_resume(
            "tmpm-scope",
            None,
            Path::new("/tmp"),
            "task",
            Some("abc-123"),
            SCOPE_SESSION_ID,
            &[],
        )
        .expect_err("a resumed pane gets the same fail-closed arm as a fresh one");

    assert_scope_failure(&err);
    assert!(
        fake.sends.lock().unwrap().is_empty(),
        "no command may reach the pane once composition failed"
    );
}

#[test]
#[serial_test::serial]
fn build_inplace_resume_command_fails_when_the_composed_file_cannot_be_written() {
    let _home = PoisonedStateRoot::set();

    let err = build_inplace_resume_command(Path::new("/tmp"), Some("abc-123"))
        .expect_err("the in-place relaunch must not drop the flag and continue");

    assert_scope_failure(&err);
}
