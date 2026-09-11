//! Argv/command-line regression tests for default-deny MCP scoping (#7422).
//!
//! Why: the scoping is only real if the flags reach the spawned process, and a
//! missing flag is silent — the session just loads everything, exactly as it
//! did before the issue. Every builder that relocates `CLAUDE_CONFIG_DIR` needs
//! its own pin, in its own test, so a future edit to one path cannot drop the
//! flags there while the others keep passing.
//! What: `spawn_command`, `resume_command` and `compose_inplace_args` — the
//! three builders in this module — asserted both ways: the flag pair present
//! when a config dir is relocated, absent when it is not.
//! Test: this file IS the test suite.

use super::*;

/// Representative managed-session UUID; not a real session.
const SCOPE_SESSION_ID: &str = "99999999-8888-7777-6666-555555555555";

/// Representative cwd; these builders are pure string composition.
const SCOPE_CWD: &str = "/tmp/scope-ws";

/// Representative relocated config dir.
const SCOPE_CONFIG_DIR: &str = "/tm/claude-config";

/// The `--mcp-config` path a session rooted at [`SCOPE_CWD`] must name.
fn expected_path() -> String {
    crate::core::session_mcp_scope::session_mcp_path(Path::new(SCOPE_CWD))
        .display()
        .to_string()
}

#[test]
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
fn compose_inplace_args_omits_the_strict_mcp_flags_without_a_config_dir() {
    let args = compose_inplace_args(Path::new(SCOPE_CWD), None, None, None);
    assert!(!args.iter().any(|a| a == "--strict-mcp-config"), "{args:?}");
}
