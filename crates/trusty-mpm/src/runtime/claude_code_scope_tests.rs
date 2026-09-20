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
//! What: `managed_launch::{spawn_spec, resume_spec}` and `compose_inplace_args`
//! asserted both ways on the flags; then `ClaudeCodeAdapter::spawn`,
//! `spawn_resume` and `build_inplace_resume_command` asserted to return `Err` —
//! and to send no command to the pane — when the composed file cannot be
//! written.
//!
//! #8233: the two shell-string builders became spec builders, so the flag
//! assertions read the resolved argv. The `--mcp-config` path is now an argv
//! TOKEN rather than a single-quoted shell word — which is the point: an argv
//! token reaches `execv` unquoted, so a path with a space no longer depends on
//! the pane shell re-splitting it correctly.
//! Test: this file IS the test suite.

use super::*;

use super::super::managed_launch::{ManagedLaunch, resume_spec, spawn_spec};
use crate::runtime::test_helpers::FakeTmux;

/// The launch these flag tests share, with `config_dir` chosen per test.
fn scope_launch(config_dir: Option<&Path>) -> ManagedLaunch<'_> {
    ManagedLaunch {
        cwd: Path::new(SCOPE_CWD),
        claude_bin: "claude",
        config_dir,
        session_id: SCOPE_SESSION_ID,
        prompt_file: None,
        oauth_token: None,
        gh_env: &[],
        mcp_env: &[],
        // #8233: the flag now renders the path the launch PROVISIONED, so these
        // builder tests supply it the same way the launch does.
        mcp_config: config_dir.map(|_| Path::new(SCOPE_MCP_CONFIG)),
        // #7685: the reachable posture — MCP scoping is orthogonal to it.
        memory_reachable: true,
    }
}

/// The composed session-MCP file [`scope_launch`] hands the builders (#8233).
const SCOPE_MCP_CONFIG: &str = "/tm/state/session-mcp/abc123.json";

/// Representative managed-session UUID; not a real session.
const SCOPE_SESSION_ID: &str = "99999999-8888-7777-6666-555555555555";

/// Representative cwd; these builders are pure string composition.
const SCOPE_CWD: &str = "/tmp/scope-ws";

/// Representative relocated config dir.
const SCOPE_CONFIG_DIR: &str = "/tm/claude-config";

/// The `--mcp-config` path a session rooted at [`SCOPE_CWD`] must name.
///
/// Why (#8233): this used to be derived from `$HOME` through `scoped_for`, which
/// [`PoisonedStateRoot`] rewrites from the fail-closed tests further down THIS
/// BINARY — so a flag test could compare against a tempdir for a reason that has
/// nothing to do with the flags. The builders now render the path the launch
/// PROVISIONED, so the expectation is that literal and reads no environment.
/// What: [`SCOPE_MCP_CONFIG`].
fn expected_path() -> String {
    SCOPE_MCP_CONFIG.to_owned()
}

/// The `--mcp-config` path the IN-PLACE relaunch must name (#8233).
///
/// Why: [`expected_path`] became a literal because `spawn_spec` and
/// `resume_spec` are handed the path the launch provisioned. `compose_inplace_args`
/// took no such argument — the bare-`tm` relaunch runs inside the managed pane,
/// so the ambient home IS its own correct layout and it still derives the path
/// itself. Comparing that derivation against the literal asserted a contract
/// this builder never had.
/// What: [`crate::core::session_mcp_scope::scoped_for`] against the same cwd and
/// config dir the builder is given, resolved at assertion time so a
/// [`PoisonedStateRoot`] sibling cannot desynchronise the two.
/// Test: `compose_inplace_args_carries_the_mcp_config_flag_unquoted`.
fn inplace_expected_path() -> String {
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
fn spawn_command_carries_the_mcp_config_flag_and_not_strict() {
    let args = spawn_spec(&scope_launch(Some(Path::new(SCOPE_CONFIG_DIR)))).args;
    let cmd = args.join(" ");
    // #7892: additive, never strict — the operator's user-scope servers load
    // beside tm's builtins.
    assert!(
        !cmd.contains("--strict-mcp-config"),
        "a relocated spawn must not narrow the operator's user scope: {cmd}"
    );
    assert!(
        cmd.contains(&format!("--mcp-config {}", expected_path())),
        "the flag must name this session's own composed file as an argv token: {cmd}"
    );
}

#[test]
#[serial_test::serial]
fn spawn_command_omits_the_mcp_config_flag_without_a_config_dir() {
    let cmd = spawn_spec(&scope_launch(None)).args.join(" ");
    assert!(
        !cmd.contains("--mcp-config"),
        "a spawn reading the operator's own ~/.claude.json is not tm's to scope: {cmd}"
    );
}

#[test]
#[serial_test::serial]
fn resume_command_carries_the_mcp_config_flag_and_not_strict() {
    let args = resume_spec(
        &scope_launch(Some(Path::new(SCOPE_CONFIG_DIR))),
        Some("abc-123"),
    )
    .args;
    let cmd = args.join(" ");
    assert!(
        !cmd.contains("--strict-mcp-config"),
        "a resumed pane must be scoped exactly like a fresh one: {cmd}"
    );
    assert!(
        cmd.contains(&format!("--mcp-config {}", expected_path())),
        "{cmd}"
    );
}

#[test]
#[serial_test::serial]
fn resume_command_omits_the_mcp_config_flag_without_a_config_dir() {
    let cmd = resume_spec(&scope_launch(None), Some("abc-123"))
        .args
        .join(" ");
    assert!(!cmd.contains("--mcp-config"), "{cmd}");
}

#[test]
#[serial_test::serial]
fn compose_inplace_args_carries_the_mcp_config_flag_unquoted() {
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
    assert!(!args.iter().any(|a| a == "--strict-mcp-config"), "{args:?}");
    assert_eq!(
        args[pos + 1],
        inplace_expected_path(),
        "exec takes the token verbatim — a quoted path names a file claude cannot open"
    );
}

#[test]
#[serial_test::serial]
fn compose_inplace_args_omits_the_mcp_config_flag_without_a_config_dir() {
    let args = compose_inplace_args(Path::new(SCOPE_CWD), None, None, None);
    assert!(!args.iter().any(|a| a == "--mcp-config"), "{args:?}");
}

// ---------------------------------------------------------------------------
// Fail-closed arms: `provision` cannot write, so no command may be built.
// ---------------------------------------------------------------------------

/// A framework root whose `session-mcp` state path is a regular FILE.
///
/// Why: every fail-closed test below needs `provision` to fail for a reason
/// that is deterministic on any machine and needs no permission games. A file
/// where `create_dir_all` must make a directory is exactly that.
///
/// // #8233: this used to redirect the PROCESS-GLOBAL `$HOME` — the class of
/// state `docs/reference/common-pitfalls.md` bans, and the reason every test in
/// this file was `#[serial]`. The adapter now takes its framework root as an
/// argument, so the poisoning is local to the returned tempdir and nothing is
/// serialized.
/// What: a tempdir holding `<tmp>/.trusty-mpm` (the root the adapter is built
/// with) with `<tmp>/.trusty-tools/trusty-mpm/session-mcp` planted as a file.
/// Test: used by every `…_fails_when_the_composed_file_cannot_be_written` test.
fn poisoned_state_root() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join(".trusty-mpm");
    let dir = crate::core::paths::FrameworkPaths::from_root(&root)
        .crate_config_root()
        .join(crate::core::session_mcp_scope::SESSION_MCP_DIR);
    std::fs::create_dir_all(dir.parent().expect("the state dir has a parent")).unwrap();
    std::fs::write(&dir, "not a directory").unwrap();
    (tmp, root)
}

/// [`poisoned_state_root`] for the one caller that still reads the process home.
///
/// Why: `build_inplace_resume_command` IS the `tm` process running inside the
/// managed pane, so its layout is the ambient one by design (#8233) — the only
/// way to poison its state root is to redirect `$HOME`. Its caller stays
/// `#[serial_test::serial]`; nothing else in this file needs to be.
/// What: redirects `$HOME` to a fresh tempdir, plants
/// `<home>/.trusty-tools/trusty-mpm/session-mcp` as a file, and restores the
/// prior `$HOME` on drop.
/// Test: `build_inplace_resume_command_fails_when_the_composed_file_cannot_be_written`.
struct PoisonedHome {
    prev: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
}

impl PoisonedHome {
    fn set() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        // SAFETY: the single caller is `#[serial]`, so no other test thread
        // races this set/restore; Drop restores it even on an unwinding panic.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let file = crate::core::session_mcp_scope::session_mcp_path(Path::new("/tmp"))
            .expect("HOME was just set");
        let dir = file.parent().expect("the composed file has a parent");
        std::fs::create_dir_all(dir.parent().expect("…which has a parent")).unwrap();
        std::fs::write(dir, "not a directory").unwrap();
        Self { prev, _tmp: tmp }
    }
}

impl Drop for PoisonedHome {
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
fn spawn_fails_when_the_composed_file_cannot_be_written() {
    let (_tmp, root) = poisoned_state_root();
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), None, &root);

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
fn spawn_resume_fails_when_the_composed_file_cannot_be_written() {
    let (_tmp, root) = poisoned_state_root();
    let fake = FakeTmux::new();
    let adapter = ClaudeCodeAdapter::new(fake.clone(), None, &root);

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
    let _home = PoisonedHome::set();

    let err = build_inplace_resume_command(Path::new("/tmp"), Some("abc-123"))
        .expect_err("the in-place relaunch must not drop the flag and continue");

    assert_scope_failure(&err);
}
