//! Tests for the #3025 `GH_TOKEN`/`GH_USER` history-safe delivery mechanism:
//! `claude_code_gh_env::{write_gh_env_file, gh_env_source_prefix}` plus their
//! wiring into `spawn_command`/`resume_command` in `claude_code.rs`.
//!
//! Why: split into a companion `_tests.rs` file (rather than growing the
//! large inline `mod tests` block in `claude_code.rs` itself) purely to keep
//! `claude_code.rs` — a production file grandfathered at a frozen SLOC budget
//! (#2398) — clear of further growth; this file is classified as a test file
//! (1500 SLOC cap) by its `_tests.rs` suffix.
//! What: proves the review-follow-up contract directly — a resolved
//! `gh_env` is written to a mode-0600 temp file as `export NAME='value'`
//! lines (never as a literal value embedded in a command string), the
//! composed `spawn_command`/`resume_command` source-and-delete that file
//! BEFORE the `env …` invocation (so the value is inherited, never typed),
//! and an EMPTY `gh_env` reproduces the exact pre-#3025 command byte-for-byte.
//! Test: itself.

use std::path::Path;

use super::claude_code_gh_env::{gh_env_source_prefix, write_gh_env_file};
use super::{resume_command, spawn_command};

/// Local copies of `mod tests`'s private `TEST_CWD`/`TEST_SESSION_ID` — this
/// file is a sibling module, not a child of `mod tests`, so it cannot reach
/// those private constants; duplicating two short literals is cheaper than
/// making them `pub(super)` just for this.
const TEST_CWD: &str = "/tmp/ws";
const TEST_SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

/// Why: an empty `gh_env` must write NOTHING — no temp file, no dangling
/// disk artifact for the common (unconfigured) case.
/// Test: itself.
#[test]
fn write_gh_env_file_empty_is_none() {
    assert!(write_gh_env_file(&[]).is_none());
}

/// Why: a non-empty `gh_env` must write one single-quoted `export NAME=value`
/// line per pair, in the SAME order `resolve_gh_account_env_with` returns
/// them (`GH_TOKEN` then `GH_USER`) — the ordering contract this module's
/// review-follow-up LOW item asks to enforce, now expressed at the file-
/// content layer since the value no longer appears in any command string.
/// Test: itself.
#[test]
fn write_gh_env_file_writes_export_lines_in_order() {
    let gh_env = vec![
        ("GH_TOKEN".to_string(), "ghp_fake_token".to_string()),
        ("GH_USER".to_string(), "bobmatnyc".to_string()),
    ];
    let file = write_gh_env_file(&gh_env).expect("file written");
    let content = std::fs::read_to_string(&file).expect("read gh-env file");
    let _ = std::fs::remove_file(&file);

    let token_pos = content
        .find("export GH_TOKEN='ghp_fake_token'")
        .expect("token line");
    let user_pos = content
        .find("export GH_USER='bobmatnyc'")
        .expect("user line");
    assert!(
        token_pos < user_pos,
        "GH_TOKEN export must precede GH_USER export: {content}"
    );
}

/// Why (Unix only): the file must be mode 0600 — owner-read/write only,
/// never group/world-readable, even momentarily. Matches the established
/// `secure_write` contract this module mirrors from `core::oauth_token`.
/// Test: itself.
#[cfg(unix)]
#[test]
fn write_gh_env_file_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let gh_env = vec![("GH_TOKEN".to_string(), "ghp_fake_token".to_string())];
    let file = write_gh_env_file(&gh_env).expect("file written");
    let mode = std::fs::metadata(&file)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    let _ = std::fs::remove_file(&file);
    assert_eq!(mode, 0o600, "gh-env file must be mode 0600, got {mode:o}");
}

/// Why: `None` (no file — the common unconfigured case) must yield an EMPTY
/// prefix, so an unconfigured project's composed command is byte-identical
/// to pre-#3025 behaviour.
/// Test: itself.
#[test]
fn gh_env_source_prefix_none_is_empty() {
    assert_eq!(gh_env_source_prefix(None), "");
}

/// Why: `Some(path)` must render a `. '<path>'; rm -f '<path>'; ` fragment —
/// SOURCING (not `cat`ing or `export`ing a literal) the file into the
/// current shell so the plain `env` invocation that follows inherits the
/// vars automatically, then deleting it immediately so the value touches
/// disk only momentarily.
/// Test: itself.
#[test]
fn gh_env_source_prefix_sources_and_deletes() {
    let prefix = gh_env_source_prefix(Some(Path::new("/tmp/tm-gh-env-test.sh")));
    assert_eq!(
        prefix,
        ". '/tmp/tm-gh-env-test.sh'; rm -f '/tmp/tm-gh-env-test.sh'; "
    );
}

/// Why: when a `gh_env_file` is supplied, `spawn_command` must source-and-
/// delete it BEFORE the `env -u ANTHROPIC_API_KEY` invocation — sourcing
/// must happen first so `env` (and therefore `claude`, and every `gh` call
/// its Bash tool makes) inherits `GH_TOKEN`/`GH_USER` from the shell's own
/// environment. This is the ordering contract that replaces the pre-review
/// "GH_TOKEN literal appears after CLAUDE_CODE_OAUTH_TOKEN" assertion: the
/// token value is no longer embedded in the command string at all (review
/// fix #2), so the meaningful invariant is now about the SOURCE STATEMENT's
/// position relative to `env`, not a literal value's position.
/// Test: itself.
#[test]
fn spawn_command_sources_gh_env_file_before_env_invocation() {
    let cmd = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        Some(Path::new("/tmp/tm-gh-env-test.sh")),
    );
    assert!(
        cmd.contains(
            ". '/tmp/tm-gh-env-test.sh'; rm -f '/tmp/tm-gh-env-test.sh'; env -u ANTHROPIC_API_KEY"
        ),
        "gh-env source-and-delete must immediately precede the env invocation: {cmd}"
    );
    // The token/account VALUE must never appear literally in the command.
    assert!(!cmd.contains("GH_TOKEN="), "cmd: {cmd}");
    assert!(!cmd.contains("GH_USER="), "cmd: {cmd}");
}

/// Why: an EMPTY `gh_env` (the common case — no `gh_account` pinned) must
/// reproduce the exact pre-#3025 command, byte-for-byte, so this feature is a
/// true no-regression addition.
/// Test: itself.
#[test]
fn spawn_command_without_gh_env_is_byte_identical_to_pre_3025() {
    let with_none = spawn_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        TEST_SESSION_ID,
        None,
        None,
        None,
    );
    assert!(!with_none.contains("gh-env"), "cmd: {with_none}");
    assert!(!with_none.contains(". '"), "cmd: {with_none}");
}

/// Why: the resume path must carry the same source-and-delete injection as
/// `spawn` — every resumed/guided-resume/crash-recovery session needs
/// identical `gh` identity delivery.
/// Test: itself.
#[test]
fn resume_command_sources_gh_env_file_before_env_invocation() {
    let cmd = resume_command(
        Path::new(TEST_CWD),
        "claude",
        None,
        None,
        false,
        TEST_SESSION_ID,
        None,
        None,
        Some(Path::new("/tmp/tm-gh-env-test.sh")),
    );
    assert!(
        cmd.contains(
            ". '/tmp/tm-gh-env-test.sh'; rm -f '/tmp/tm-gh-env-test.sh'; env -u ANTHROPIC_API_KEY"
        ),
        "gh-env source-and-delete must immediately precede the env invocation: {cmd}"
    );
}
