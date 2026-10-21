//! Coverage for the lean `session_context_catchup` default (#9883).
//!
//! Why: #9883 made the default resume answer the resolved snapshot plus
//! live git status, with the digest behind `full: true`, and merged
//! `full`'s old "ignore the watermark" meaning into that flag.
//! What: drives [`session_context_catchup_in`] on an isolated host against a
//! real temp checkout — one test per #9883 criterion the backend owns.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use trusty_common::catchup::pause::{PauseSnapshotInput, write_pause_snapshot};

use crate::daemon::catchup_host::CatchupHost;
use crate::daemon::mcp_context::session_context_catchup_in;
use crate::daemon::resume_verify::ResumeVerifyHost;
use crate::daemon::resume_verify_runner::fake::FakeRunner;

/// The digest keys a lean answer must not carry (#9883 criterion 1).
const DIGEST_KEYS: [&str; 3] = ["sessions", "recent_commits", "recent_memory"];

/// A host that reaches no operator config, memory daemon, `gh` or `git` verify.
fn isolated_host(tmp: &Path) -> CatchupHost {
    CatchupHost {
        config: crate::core::config::MpmConfig::default(),
        memory_socket: tmp.join("nothing-serves-this.sock"),
        resume_verify: ResumeVerifyHost {
            runner: std::sync::Arc::new(FakeRunner::never()),
            palace: Some("catchup-lean-test".to_string()),
            command_timeout: Duration::from_secs(1),
            pass_timeout: Duration::from_secs(2),
        },
    }
}

/// `git -C <dir>` with no global or system config (#9883): a host's
/// `commit.gpgsign` or `status.showUntrackedFiles` must not reach a fixture.
fn fixture_git(dir: &Path) -> std::process::Command {
    let mut cmd = trusty_common::git::command_in(dir);
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

/// A one-commit checkout at `<tmp>/repo`.
fn checkout(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "lean@example.invalid"],
        vec!["config", "user.name", "Lean Tester"],
        // The probe under test runs with host config; a local value wins.
        vec!["config", "status.showUntrackedFiles", "normal"],
    ] {
        let out = fixture_git(&repo).args(&args).output().unwrap();
        assert!(out.status.success(), "fixture `git {args:?}` failed");
    }
    std::fs::write(repo.join("a.txt"), b"a").unwrap();
    for args in [vec!["add", "a.txt"], vec!["commit", "-m", "init"]] {
        let out = fixture_git(&repo).args(&args).output().unwrap();
        assert!(out.status.success(), "fixture `git {args:?}` failed");
    }
    repo
}

/// Pause `session_id` in `repo` with `summary`; returns the snapshot path.
fn pause(repo: &Path, session_id: &str, summary: &str) -> String {
    let next = vec![format!("{session_id}: ship it")];
    let input = PauseSnapshotInput {
        session_id,
        summary,
        completed: &[],
        in_progress: &[],
        next_steps: &next,
        tmux_window: None,
    };
    let outcome = write_pause_snapshot(repo, &input).unwrap();
    outcome.snapshot_path.display().to_string()
}

/// One catch-up as `session_id`, `full` as given.
async fn catchup(host: &CatchupHost, repo: &Path, session_id: &str, full: bool) -> Value {
    session_context_catchup_in(
        host,
        repo.to_str().unwrap(),
        Some(session_id),
        None,
        None,
        false,
        full,
        0,
    )
    .await
    .expect("the catch-up answers")
}

/// Why (#9883 criterion 1): "Default call returns the resolved snapshot and
/// git status only."
/// What: a paused session with an uncommitted file catches up with
/// `full: false`. The answer names the snapshot, carries its recorded fields
/// (`resolved_session`, pause-time `git_context` included), and a LIVE
/// `git_status` listing the dirty file — and none of the digest keys.
/// Test: itself.
#[tokio::test]
async fn the_default_returns_the_resolved_snapshot_and_live_git_status_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = checkout(tmp.path());
    let snapshot = pause(&repo, "s-lean", "Lean resume work.");
    std::fs::write(repo.join("dirty.txt"), b"uncommitted").unwrap();

    let body = catchup(&isolated_host(tmp.path()), &repo, "s-lean", false).await;

    assert_eq!(body["resolved_snapshot"], snapshot, "{body}");
    assert_eq!(body["resolved_via"], "session_id", "{body}");
    let session = &body["resolved_session"];
    assert_eq!(session["summary"], "Lean resume work.", "{body}");
    assert!(session["git_context"].is_string(), "{body}");
    let status = &body["git_status"];
    assert_eq!(status["available"], true, "{body}");
    let entries: Vec<&str> = status["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("git_status.entries must be an array: {body}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(entries.contains(&"?? dirty.txt"), "{entries:?}");
    for key in DIGEST_KEYS {
        assert!(
            body.get(key).is_none(),
            "lean answer carries `{key}`: {body}"
        );
    }
    // Unchanged #7501 fields ride along.
    assert!(body["resolved_snapshot_superseded"].is_boolean(), "{body}");
    assert!(body["commits_since_snapshot"].is_array(), "{body}");
}

/// Why (#9883 criterion 2): "The full digest is returned only with `--full` /
/// `full: true`."
/// What: the same checkout answers the three digest keys under `full: true`,
/// exactly as before (no lean-only key added), and never under `full: false`.
/// Test: itself.
#[tokio::test]
async fn only_full_true_returns_the_digest() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = checkout(tmp.path());
    pause(&repo, "s-full", "Full digest work.");
    let host = isolated_host(tmp.path());

    let full = catchup(&host, &repo, "s-full", true).await;
    for key in DIGEST_KEYS {
        assert!(full[key].is_array(), "full answer lacks `{key}`: {full}");
    }
    assert_eq!(full["sessions_total"], 1, "{full}");
    assert!(full.get("git_status").is_none(), "{full}");
    assert!(full.get("resolved_session").is_none(), "{full}");

    let lean = catchup(&host, &repo, "s-full", false).await;
    for key in DIGEST_KEYS {
        assert!(
            lean.get(key).is_none(),
            "default answer carries `{key}`: {lean}"
        );
    }
}

/// Why (#9883 criterion 3): the ownership checks (#5272, #5386) and the
/// session-ref hydration pass (ADR-0062, #7830) are unchanged under the lean
/// default.
/// What: A and B pause in one store. B's default catch-up resolves nothing and
/// carries no trace of A's snapshot; A's resolves A's own file; both carry the
/// hydration receipt.
/// Test: itself.
#[tokio::test]
async fn the_lean_default_keeps_the_ownership_and_hydration_checks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = checkout(tmp.path());
    let a_snapshot = pause(&repo, "s-owner-a", "Session A's private work.");
    let host = isolated_host(tmp.path());

    let for_b = catchup(&host, &repo, "s-owner-b", false).await;
    assert!(for_b["resolved_snapshot"].is_null(), "{for_b}");
    assert!(for_b["resolved_session"].is_null(), "{for_b}");
    let text = for_b.to_string();
    assert!(!text.contains("Session A's private work."), "{text}");
    assert!(!text.contains(&a_snapshot), "{text}");
    assert!(for_b["session_refs"]["hydrated"].is_boolean(), "{for_b}");

    let for_a = catchup(&host, &repo, "s-owner-a", false).await;
    assert_eq!(for_a["resolved_snapshot"], a_snapshot, "{for_a}");
    assert_eq!(
        for_a["resolved_session"]["summary"], "Session A's private work.",
        "{for_a}"
    );
    assert!(for_a["session_refs"]["hydrated"].is_boolean(), "{for_a}");
    assert_eq!(for_a["watermark_advanced"], false, "{for_a}");
}
