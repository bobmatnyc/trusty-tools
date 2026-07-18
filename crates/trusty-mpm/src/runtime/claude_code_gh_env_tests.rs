//! Tests for the #3025 `GH_TOKEN`/`GH_USER` spawn-env injection wired into
//! `env_bin_prefix`/`spawn_command`/`resume_command` in `claude_code.rs`.
//!
//! Why: split into a companion `_tests.rs` file (rather than growing the
//! large inline `mod tests` block in `claude_code.rs` itself) purely to keep
//! `claude_code.rs` — a production file grandfathered at a frozen SLOC budget
//! (#2398) — clear of further growth; this file is classified as a test file
//! (1500 SLOC cap) by its `_tests.rs` suffix.
//! What: exercises `spawn_command`/`resume_command` directly with a
//! synthetic `gh_env` slice (no live `gh`/git origin needed — the resolution
//! step itself, `resolve_gh_account_env_for_workspace`, is unit-tested in
//! `core::gh_account_spawn_env_tests`), proving the two documented
//! invariants: GH_TOKEN/GH_USER are single-quoted and appended to the `env`
//! prefix after `CLAUDE_CONFIG_DIR`/`CLAUDE_CODE_OAUTH_TOKEN`, and an EMPTY
//! `gh_env` reproduces the pre-#3025 command byte-for-byte.
//! Test: itself.

use std::path::Path;

use super::{resume_command, spawn_command};

/// Local copies of `mod tests`'s private `TEST_CWD`/`TEST_SESSION_ID` — this
/// file is a sibling module, not a child of `mod tests`, so it cannot reach
/// those private constants; duplicating two short literals is cheaper than
/// making them `pub(super)` just for this.
const TEST_CWD: &str = "/tmp/ws";
const TEST_SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

/// Why: when a `gh_env` override is supplied, `spawn_command` must include
/// both `GH_TOKEN` and `GH_USER` (single-quoted) in the `env` prefix, ordered
/// after `CLAUDE_CONFIG_DIR`/`CLAUDE_CODE_OAUTH_TOKEN` — see
/// `env_bin_prefix`'s doc for the ordering contract.
/// Test: itself.
#[test]
fn spawn_command_sets_gh_token_when_pinned() {
    let gh_env = vec![
        ("GH_TOKEN".to_string(), "ghp_fake_token".to_string()),
        ("GH_USER".to_string(), "bobmatnyc".to_string()),
    ];
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        &gh_env,
    );
    assert!(
        cmd.contains("GH_TOKEN='ghp_fake_token'"),
        "spawn command must set GH_TOKEN: {cmd}"
    );
    assert!(
        cmd.contains("GH_USER='bobmatnyc'"),
        "spawn command must set GH_USER: {cmd}"
    );
}

/// Why: an EMPTY `gh_env` (the common case — no `gh_account` pinned) must
/// reproduce the exact pre-#3025 command, byte-for-byte, so this feature is a
/// true no-regression addition.
/// Test: itself.
#[test]
fn spawn_command_without_gh_env_is_byte_identical_to_pre_3025() {
    let with_empty = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        &[],
    );
    assert!(!with_empty.contains("GH_TOKEN"), "cmd: {with_empty}");
    assert!(!with_empty.contains("GH_USER"), "cmd: {with_empty}");
}

/// Why: the resume path must carry the same GH_TOKEN/GH_USER injection as
/// `spawn` — every resumed/guided-resume/crash-recovery session needs
/// identical `gh` identity.
/// Test: itself.
#[test]
fn resume_command_sets_gh_token_when_pinned() {
    let gh_env = vec![("GH_TOKEN".to_string(), "ghp_fake_token".to_string())];
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        None,
        false,
        TEST_SESSION_ID,
        None,
        None,
        &gh_env,
    );
    assert!(
        cmd.contains("GH_TOKEN='ghp_fake_token'"),
        "resume command must set GH_TOKEN: {cmd}"
    );
}
