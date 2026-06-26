//! Unit tests for the guided-default session-picker UX (#1705).
//!
//! Why: the guided-default must correctly detect GitHub projects, show a
//! readable session list, gracefully degrade for non-TTY callers, and
//! correctly derive the managed workspace path. These properties can be
//! checked without a live daemon.
//! What: tests for `derive_project`, `parse_picker_choice`, `tty_gate`,
//! `print_project_context`, `print_non_tty_hint`, and the fallback path.
//! Test: `cargo test -p trusty-mpm -- tests_behavior_c` runs this suite;
//! no network or tmux required.

use std::path::PathBuf;

use crate::commands::guided::{
    PickerDecision, derive_project, fallback_protected, parse_picker_choice, print_non_tty_hint,
    print_project_context, tty_gate,
};

// ── parse_picker_choice ───────────────────────────────────────────────────────

#[test]
fn guided_picker_bare_enter_no_sessions_launches_new() {
    // Why: bare Enter with no sessions must launch a new session, not hang.
    assert_eq!(parse_picker_choice("", 0), PickerDecision::LaunchNew);
    assert_eq!(parse_picker_choice("  \t", 0), PickerDecision::LaunchNew);
}

#[test]
fn guided_picker_bare_enter_with_sessions_resumes_first() {
    // Why: bare Enter when sessions exist must resume the most-recent session
    // (index 0) — the documented default for sessions-present mode.
    assert_eq!(parse_picker_choice("", 1), PickerDecision::Resume(0));
    assert_eq!(parse_picker_choice("  ", 3), PickerDecision::Resume(0));
}

#[test]
fn guided_picker_q_returns_quit() {
    // Why: "q" must quit cleanly without touching tmux or the daemon.
    assert_eq!(parse_picker_choice("q", 0), PickerDecision::Quit);
    assert_eq!(parse_picker_choice("q", 3), PickerDecision::Quit);
}

#[test]
fn guided_picker_q_uppercase_returns_quit() {
    // Why: "Q" must be treated identically to "q" (case-insensitive).
    assert_eq!(parse_picker_choice("Q", 2), PickerDecision::Quit);
    assert_eq!(parse_picker_choice("Q\n", 0), PickerDecision::Quit);
}

#[test]
fn guided_picker_numeric_valid_resumes() {
    // Why: "[N]" where 1 <= N <= session_count must resume the Nth session (0-based).
    assert_eq!(parse_picker_choice("1", 1), PickerDecision::Resume(0));
    assert_eq!(parse_picker_choice("1", 3), PickerDecision::Resume(0));
    assert_eq!(parse_picker_choice("2", 3), PickerDecision::Resume(1));
    assert_eq!(parse_picker_choice("3", 3), PickerDecision::Resume(2));
    // With newline (as stdin read_line returns)
    assert_eq!(parse_picker_choice("2\n", 3), PickerDecision::Resume(1));
}

#[test]
fn guided_picker_numeric_launch_new() {
    // Why: "[session_count+1]" must always launch a new session.
    assert_eq!(parse_picker_choice("1", 0), PickerDecision::LaunchNew);
    assert_eq!(parse_picker_choice("4", 3), PickerDecision::LaunchNew);
}

#[test]
fn guided_picker_out_of_range_unrecognised() {
    // Why: a number out of range (>session_count+1) must not silently
    // resume or launch — it must be rejected cleanly.
    assert_eq!(parse_picker_choice("5", 3), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("100", 1), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("0", 3), PickerDecision::Unrecognised);
}

#[test]
fn guided_picker_non_numeric_unrecognised() {
    // Why: arbitrary text input must be rejected without panicking.
    assert_eq!(parse_picker_choice("abc", 2), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("exit", 0), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("1a", 3), PickerDecision::Unrecognised);
}

// ── tty_gate ──────────────────────────────────────────────────────────────────

#[test]
fn guided_non_tty_gate_returns_false_skips_stdin() {
    // Why: when is_tty=false the function must return false so the caller
    // returns Ok(()) without ever reading from stdin — the core of AC-7.
    // This test exercises the non-TTY branch without any live stdin.
    let result = tty_gate(false, "owner/repo", &PathBuf::from("/ws/owner/repo"), &[]);
    assert!(
        !result,
        "non-TTY gate must return false (no picker) for empty sessions"
    );
}

#[test]
fn guided_tty_gate_returns_true_for_tty() {
    // Why: when is_tty=true the function must return true so the caller
    // proceeds to run_tty_picker.
    let result = tty_gate(true, "owner/repo", &PathBuf::from("/ws/owner/repo"), &[]);
    assert!(
        result,
        "TTY gate must return true so caller runs the picker"
    );
}

#[test]
fn guided_non_tty_gate_returns_false_with_sessions() {
    // Why: the non-TTY branch must work even when sessions are present —
    // ensuring the hint path handles the session list safely.
    let sessions = vec![make_session("tm-api-1", "running", None)];
    let result = tty_gate(false, "owner/repo", &PathBuf::from("/ws"), &sessions);
    assert!(
        !result,
        "non-TTY gate must return false regardless of session count"
    );
}

// ── derive_project ────────────────────────────────────────────────────────────

#[test]
fn guided_derive_project_returns_none_for_non_git_dir() {
    // Why: a plain temp directory (not a git repo) should not yield a project.
    // What: derive_project(temp_dir) must return None.
    let tmp = std::env::temp_dir();
    let non_git = tmp.join("trusty_test_non_git_dir_1705");
    std::fs::create_dir_all(&non_git).ok();
    let result = derive_project(&non_git);
    assert!(
        result.is_none(),
        "expected None for non-git dir, got {result:?}"
    );
}

#[test]
fn guided_derive_project_rejects_non_github_remote() {
    // Why: if the origin is not a GitHub URL, derive_project must return None
    // so the live-checkout guard fires downstream.
    let tmp = tempdir_with_name("trusty_test_non_github_remote_1705");
    let ok = git_init_quiet(&tmp);
    if !ok {
        return; // git unavailable
    }
    git_remote_add(&tmp, "https://gitlab.com/owner/repo.git");
    let result = derive_project(&tmp);
    assert!(
        result.is_none(),
        "expected None for non-GitHub remote (gitlab), got {result:?}"
    );
}

#[test]
fn guided_derive_project_accepts_github_https_remote() {
    // Why: a valid HTTPS GitHub remote must parse correctly and return the
    // expected source_id, a non-empty workspace path, and the git root.
    let tmp = tempdir_with_name("trusty_test_github_https_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "https://github.com/owner/my-repo.git");
    let result = derive_project(&tmp);
    match result {
        Some((source_id, workspace, git_root)) => {
            assert_eq!(source_id, "owner/my-repo");
            assert!(
                !workspace.as_os_str().is_empty(),
                "workspace must be non-empty"
            );
            // workspace must be the managed clone path, not the live checkout
            assert_ne!(workspace, tmp, "workspace must differ from live checkout");
            // git_root must resolve to tmp (the repo root). On macOS /var is a
            // symlink to /private/var; git resolves the canonical path, so compare
            // canonicalized forms.
            let canonical_root = git_root.canonicalize().unwrap_or(git_root);
            let canonical_tmp = tmp.canonicalize().unwrap_or(tmp.clone());
            assert_eq!(
                canonical_root, canonical_tmp,
                "git_root must be the repo root"
            );
        }
        None => panic!("expected Some for GitHub HTTPS remote, got None"),
    }
}

#[test]
fn guided_derive_project_accepts_github_ssh_remote() {
    // Why: SSH-style GitHub remotes (`git@github.com:owner/repo.git`) must be
    // detected in the same way as HTTPS remotes.
    let tmp = tempdir_with_name("trusty_test_github_ssh_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "git@github.com:owner/my-repo.git");
    let result = derive_project(&tmp);
    match result {
        Some((source_id, _workspace, _git_root)) => {
            assert_eq!(source_id, "owner/my-repo");
        }
        None => panic!("expected Some for GitHub SSH remote, got None"),
    }
}

#[test]
fn guided_derive_project_returns_some_from_subdir() {
    // Why: derive_project must work when called from a subdirectory of a git
    // repo, and the returned git_root must be the repo root (not the subdir)
    // so that `launch_new_session_and_attach` passes the git root as repo_url
    // and the daemon finds .git, sets source_id correctly (#1705 LOW fix).
    let tmp = tempdir_with_name("trusty_test_subdir_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "https://github.com/owner/my-repo.git");

    // Create a nested subdirectory and call derive_project from it.
    let subdir = tmp.join("src").join("lib");
    std::fs::create_dir_all(&subdir).unwrap();

    let result = derive_project(&subdir);
    match result {
        Some((source_id, _workspace, git_root)) => {
            assert_eq!(source_id, "owner/my-repo");
            // git_root must be the repo root (tmp), NOT the subdir. On macOS
            // /var is a symlink to /private/var; compare canonical forms.
            let canonical_root = git_root.canonicalize().unwrap_or(git_root);
            let canonical_tmp = tmp.canonicalize().unwrap_or(tmp.clone());
            assert_eq!(
                canonical_root, canonical_tmp,
                "git_root from subdir must be repo root, not the nested dir"
            );
        }
        None => panic!("expected Some when calling derive_project from a subdir"),
    }
}

// ── print_project_context / print_non_tty_hint ────────────────────────────────

#[test]
fn guided_print_project_context_does_not_panic_no_sessions() {
    // Why: the display helper must not panic when the session list is empty.
    print_project_context(
        "owner/repo",
        &PathBuf::from("/home/user/repos/owner/repo"),
        &[],
    );
}

#[test]
fn guided_print_project_context_does_not_panic_with_sessions() {
    // Why: the display helper must not panic when optional fields are None.
    let sessions = vec![make_session(
        "tm-frontend-1",
        "running",
        Some("2026-06-25T12:00:00Z"),
    )];
    print_project_context(
        "owner/repo",
        &PathBuf::from("/home/user/repos/owner/repo"),
        &sessions,
    );
}

#[test]
fn guided_print_non_tty_hint_does_not_panic_no_sessions() {
    // Why: the non-TTY degradation path must work when there are no sessions.
    print_non_tty_hint("owner/repo", &[]);
}

#[test]
fn guided_print_non_tty_hint_does_not_panic_with_sessions() {
    // Why: the non-TTY hint must print the session name for a resume hint.
    let sessions = vec![make_session("tm-api-2", "stopped", None)];
    print_non_tty_hint("owner/repo", &sessions);
}

// ── fallback_protected in non-git dir ────────────────────────────────────────

#[tokio::test]
async fn guided_fallback_non_git_dir_calls_launch_path() {
    // Why: for a non-git directory, fallback_protected should call launch()
    // which will fail (daemon not running) rather than returning a
    // "live git checkout protected" error.
    let tmp = tempdir_with_name("trusty_test_fallback_nongit_1705");
    let client = reqwest::Client::new();
    let result = fallback_protected(&client, "http://127.0.0.1:19999", &tmp).await;
    // The function should NOT return the live-checkout protection error.
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(
            !msg.contains("live git checkout"),
            "non-git dir should NOT trigger live-checkout guard; got: {msg}"
        );
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Create (or replace) a temp directory with the given name under the OS temp dir.
fn tempdir_with_name(name: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(name);
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).expect("create temp dir");
    tmp
}

/// Run `git init -q` in `dir`. Returns true on success, false if git unavailable.
fn git_init_quiet(dir: &PathBuf) -> bool {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `git init` + minimal identity config + empty initial commit.
/// Returns true on success, false if git is unavailable in this environment.
fn git_init_with_commit(dir: &PathBuf) -> bool {
    if !git_init_quiet(dir) {
        return false;
    }
    // Configure a minimal identity so `git commit` doesn't fail.
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(dir)
            .status()
            .ok();
    }
    // Empty initial commit so the repo has a valid HEAD ref.
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init", "-q"])
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Add `remote.origin` with the given URL.
fn git_remote_add(dir: &PathBuf, url: &str) {
    std::process::Command::new("git")
        .args(["remote", "add", "origin", url])
        .current_dir(dir)
        .status()
        .ok();
}

/// Construct a minimal `ManagedSessionSummary` for tests.
fn make_session(
    name: &str,
    state: &str,
    last_activity_at: Option<&str>,
) -> trusty_mpm::client::ManagedSessionSummary {
    trusty_mpm::client::ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: last_activity_at.map(str::to_owned),
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        claude_session_id: None,
    }
}
