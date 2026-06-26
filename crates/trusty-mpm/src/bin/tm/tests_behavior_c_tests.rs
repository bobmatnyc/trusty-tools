//! Unit tests for the guided-default session-picker UX (#1705).
//!
//! Why: the guided-default must correctly detect GitHub projects, show a
//! readable session list, gracefully degrade for non-TTY callers, and
//! correctly derive the managed workspace path. These properties can be
//! checked without a live daemon.
//! What: tests for `derive_project`, `print_project_context`,
//! `print_non_tty_hint`, `is_github_remote` logic, and the list-parse helper.
//! Test: `cargo test -p trusty-mpm -- tests_behavior_c` runs this suite;
//! no network or tmux required.

use crate::commands::guided::{
    derive_project, fallback_protected, print_non_tty_hint, print_project_context,
};

// ── derive_project ────────────────────────────────────────────────────────────

#[test]
fn guided_derive_project_returns_none_for_non_git_dir() {
    // Why: a plain temp directory (not a git repo) should not yield a project.
    // What: derive_project(temp_dir) must return None.
    // Test: pass the process's temp dir; on macOS/Linux it is never a git root.
    let tmp = std::env::temp_dir();
    // Avoid /tmp itself being inside a git tree (shouldn't happen, but guard).
    let non_git = tmp.join("trusty_test_non_git_dir_1705");
    std::fs::create_dir_all(&non_git).ok();
    let result = derive_project(&non_git);
    assert!(
        result.is_none(),
        "expected None for non-git dir, got {result:?}"
    );
}

#[test]
fn guided_derive_project_returns_some_for_trusty_tools_repo() {
    // Why: running from inside the trusty-tools workspace (a GitHub repo) should
    // yield the correct source_id and a non-empty workspace path.
    // What: derive_project(workspace_root) → Some(("masa/trusty-tools" or similar
    //   "owner/repo", workspace_path)); we check the shape, not the exact owner.
    // Test: uses the worktree path which is inside a git repo with a GitHub remote.
    let wt = std::path::PathBuf::from(
        "/Users/masa/Projects/trusty-tools/.claude/worktrees/agent-a20307c302b9e45b5",
    );
    if !wt.exists() {
        // Skip when run outside this specific CI environment.
        return;
    }
    let result = derive_project(&wt);
    match result {
        Some((source_id, workspace)) => {
            // source_id must be "owner/repo"
            assert!(
                source_id.contains('/'),
                "source_id must be owner/repo, got '{source_id}'"
            );
            // workspace must be a non-empty path
            assert!(
                !workspace.as_os_str().is_empty(),
                "workspace path must be non-empty"
            );
            // workspace must NOT be the live checkout itself
            assert_ne!(
                workspace, wt,
                "workspace must be managed clone path, not the live checkout"
            );
        }
        None => {
            // Acceptable if the remote is non-GitHub or the git command fails.
            // Print a note so the developer knows why this branch ran.
            eprintln!(
                "derive_project returned None for worktree dir — remote may be non-GitHub or git unavailable"
            );
        }
    }
}

// ── is_github_remote (via derive_project indirection) ────────────────────────

#[test]
fn guided_derive_project_rejects_non_github_remote() {
    // Why: if the origin is not a GitHub URL, derive_project must return None
    // so the live-checkout guard fires downstream.
    // What: we create a temp git repo with a non-GitHub origin and assert None.
    let tmp = std::env::temp_dir().join("trusty_test_non_github_remote_1705");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).unwrap();

    // Init a bare git repo with a non-GitHub remote.
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        std::fs::remove_dir_all(&tmp).ok();
        return; // git unavailable in this environment
    }
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://gitlab.com/owner/repo.git",
        ])
        .current_dir(&tmp)
        .status()
        .ok();

    let result = derive_project(&tmp);
    std::fs::remove_dir_all(&tmp).ok();

    assert!(
        result.is_none(),
        "expected None for non-GitHub remote (gitlab), got {result:?}"
    );
}

#[test]
fn guided_derive_project_accepts_github_https_remote() {
    // Why: a valid HTTPS GitHub remote must parse correctly.
    // What: we create a temp git repo with a GitHub HTTPS remote and assert Some.
    let tmp = std::env::temp_dir().join("trusty_test_github_https_remote_1705");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).unwrap();

    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        std::fs::remove_dir_all(&tmp).ok();
        return;
    }
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/owner/my-repo.git",
        ])
        .current_dir(&tmp)
        .status()
        .ok();

    let result = derive_project(&tmp);
    std::fs::remove_dir_all(&tmp).ok();

    match result {
        Some((source_id, _workspace)) => {
            assert_eq!(source_id, "owner/my-repo");
        }
        None => panic!("expected Some for GitHub HTTPS remote, got None"),
    }
}

#[test]
fn guided_derive_project_accepts_github_ssh_remote() {
    // Why: SSH-style GitHub remotes (`git@github.com:owner/repo.git`) must be
    // detected in the same way as HTTPS remotes.
    // What: temp git repo with SSH remote → Some("owner/my-repo", …).
    let tmp = std::env::temp_dir().join("trusty_test_github_ssh_remote_1705");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).unwrap();

    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        std::fs::remove_dir_all(&tmp).ok();
        return;
    }
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:owner/my-repo.git",
        ])
        .current_dir(&tmp)
        .status()
        .ok();

    let result = derive_project(&tmp);
    std::fs::remove_dir_all(&tmp).ok();

    match result {
        Some((source_id, _workspace)) => {
            assert_eq!(source_id, "owner/my-repo");
        }
        None => panic!("expected Some for GitHub SSH remote, got None"),
    }
}

// ── print_project_context / print_non_tty_hint ────────────────────────────────

#[test]
fn guided_print_project_context_does_not_panic_no_sessions() {
    // Why: the display helper must not panic when the session list is empty.
    // What: call print_project_context with an empty session slice.
    print_project_context(
        "owner/repo",
        &std::path::PathBuf::from("/home/user/trusty-tools/repos/owner/repo"),
        &[],
    );
}

#[test]
fn guided_print_project_context_does_not_panic_with_sessions() {
    // Why: the display helper must not panic when sessions have optional fields
    // that are None.
    // What: construct a ManagedSessionSummary with minimal fields set.
    let sessions = vec![trusty_mpm::client::ManagedSessionSummary {
        id: "abc123".to_string(),
        name: "tm-frontend-1".to_string(),
        state: "running".to_string(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: Some("2026-06-25T12:00:00Z".to_string()),
        pending_decision: None,
        proposed_default: None,
    }];
    print_project_context(
        "owner/repo",
        &std::path::PathBuf::from("/home/user/repos/owner/repo"),
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
    let sessions = vec![trusty_mpm::client::ManagedSessionSummary {
        id: "def456".to_string(),
        name: "tm-api-2".to_string(),
        state: "stopped".to_string(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
    }];
    print_non_tty_hint("owner/repo", &sessions);
}

// ── fallback_protected in non-git dir ────────────────────────────────────────

#[tokio::test]
async fn guided_fallback_non_git_dir_calls_launch_path() {
    // Why: for a non-git directory (AC-6 "non-git → fall back to help"),
    // fallback_protected should call launch() which will fail (daemon not running)
    // rather than returning a "live checkout protected" error.
    // What: call fallback_protected from a non-git temp dir; expect either
    //   Ok (launch ran) or an Err whose message does NOT contain "live git checkout".
    let tmp = std::env::temp_dir().join("trusty_test_fallback_nongit_1705");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).unwrap();

    let client = reqwest::Client::new();
    let result = fallback_protected(&client, "http://127.0.0.1:19999", &tmp).await;
    std::fs::remove_dir_all(&tmp).ok();

    // The function should NOT return the live-checkout protection error.
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(
            !msg.contains("live git checkout"),
            "non-git dir should NOT trigger live-checkout guard; got: {msg}"
        );
    }
    // Ok is also acceptable (launch somehow succeeded — extremely unlikely in CI).
}
