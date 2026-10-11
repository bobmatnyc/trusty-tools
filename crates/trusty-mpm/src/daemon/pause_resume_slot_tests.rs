//! Coverage for the pause's resume slot (#9390).
//!
//! Why: a pause must leave identifiers and a re-read command behind, never a
//! head SHA, and a dead memory socket must never cost the operator the pause.
//! What: drives `session_context_pause_with` against a recording stub daemon on
//! a temp socket (the same `uds_mock` the memory verbs use), and against a
//! socket nothing serves.
//! Test: this file.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::ResumeSlotHost;
use crate::daemon::mcp_context::session_context_pause_with;
use crate::daemon::state::DaemonState;

/// Every `(method, params)` the stub daemon was sent.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A stub daemon answering `answer` to every call, recording each request.
async fn recording_daemon(
    answer: Result<Value, (i64, String)>,
) -> (crate::uds_mock::MockUdsDaemon, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let daemon = crate::uds_mock::spawn(move |method: &str, params: Value| {
        if let Ok(mut seen) = seen.lock() {
            seen.push((method.to_string(), params));
        }
        let answer = answer
            .clone()
            .map_err(|(code, msg)| crate::uds_mock::RpcError::new(code, msg));
        Box::pin(async move { answer })
    })
    .await;
    (daemon, calls)
}

fn git(dir: &Path, args: &[&str]) {
    let out = trusty_common::git::command_in(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "fixture `git {args:?}` failed");
}

/// A checkout on `feat/9390-x` that git-ignores its session store, so the
/// pause's PR publish skips instead of reaching a remote.
fn repo_on_branch(dir: &Path) {
    git(dir, &["init", "--initial-branch=main"]);
    git(dir, &["config", "user.email", "pause@example.invalid"]);
    git(dir, &["config", "user.name", "Pause Tester"]);
    std::fs::write(dir.join(".gitignore"), ".trusty-mpm/\n").unwrap();
    git(dir, &["add", ".gitignore"]);
    git(dir, &["commit", "-m", "first"]);
    git(dir, &["checkout", "-b", "feat/9390-x"]);
}

/// One pause whose PM text names two PRs and a head SHA.
async fn pause(dir: &Path, memory: &ResumeSlotHost) -> Value {
    session_context_pause_with(
        &DaemonState::shared(),
        dir.to_str().unwrap(),
        Some("s-slot"),
        "PR #12 head d3963848ab is green on feat/9390-x.",
        vec!["merged PR #34 at abc1234".to_string()],
        vec![],
        vec![],
        None,
        false,
        false,
        memory,
    )
    .await
    .expect("a pause never fails on its memory leg")
}

/// Every 7-40 character hex token holding a letter, in `text`.
fn sha_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| {
            (7..=40).contains(&w.len())
                && w.chars().all(|c| c.is_ascii_hexdigit())
                && w.chars().any(|c| c.is_ascii_alphabetic())
        })
        .map(str::to_string)
        .collect()
}

/// Why (#9390): the resume slot is what the next session re-verifies, so it
/// must carry the slot key, an expiry and a command built from identifiers,
/// and no head SHA copied from the PM's text.
/// What: one pause against a recording stub; the single `memory_remember`
/// request carries `fact_key`, a future `expires_at`, the per-identifier
/// `verify` lines and the snapshot path, and no SHA-like token anywhere.
#[tokio::test]
async fn pause_writes_a_resume_slot_with_identifiers_and_a_verify_command() {
    let tmp = tempfile::TempDir::new().unwrap();
    repo_on_branch(tmp.path());
    let stored = json!({ "drawer_id": "d1", "palace": "p", "status": "stored", "tier": "C" });
    let (daemon, calls) = recording_daemon(Ok(stored)).await;
    let memory = ResumeSlotHost {
        socket: daemon.socket().to_path_buf(),
        palace: Some("p".to_string()),
    };

    let result = pause(tmp.path(), &memory).await;

    assert_eq!(result["resume_slot"], json!("stored"), "{result}");
    assert!(Path::new(result["snapshot_path"].as_str().unwrap()).exists());
    let seen = calls.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "exactly one write: {seen:?}");
    let (method, params) = &seen[0];
    assert_eq!(method, "memory_remember");
    assert_eq!(params["palace"], json!("p"));
    assert_eq!(params["fact_key"], json!("ws:s-slot/resume"));
    let expires = params["expires_at"].as_str().expect("expires_at");
    let expires = chrono::DateTime::parse_from_rfc3339(expires).expect("RFC 3339");
    assert!(expires > chrono::Utc::now(), "a future expiry: {expires}");
    assert_eq!(
        params["verify"],
        json!(
            "gh pr view 12 --json headRefOid,state\n\
             gh pr view 34 --json headRefOid,state\n\
             git rev-parse feat/9390-x"
        )
    );
    let text = params["text"].as_str().expect("text");
    for identifier in ["feat/9390-x", "#12", "#34", ".trusty-mpm/sessions/s-slot/"] {
        assert!(text.contains(identifier), "{identifier:?} in {text:?}");
    }
    assert_eq!(
        sha_tokens(&params.to_string()),
        Vec::<String>::new(),
        "no head SHA in the slot request: {params}"
    );
}

/// Why (#9390): the snapshot file is the pause's primary write; a memory
/// daemon that is down must cost nothing but the slot.
/// What: a socket nothing serves; the pause returns `Ok`, the snapshot exists,
/// and `resume_slot` reports the skip and its reason.
#[tokio::test]
async fn pause_survives_an_unreachable_memory_socket() {
    let tmp = tempfile::TempDir::new().unwrap();
    repo_on_branch(tmp.path());
    let memory = ResumeSlotHost {
        socket: tmp.path().join("nothing-listens.sock"),
        palace: Some("p".to_string()),
    };

    let result = pause(tmp.path(), &memory).await;

    assert!(Path::new(result["snapshot_path"].as_str().unwrap()).exists());
    assert_eq!(
        result["resume_slot"],
        json!("skipped:memory_unreachable"),
        "{result}"
    );
}

/// Why (#9390): PR B stores the drawer even when its verify command fails to
/// record, and says so with `VerifyNotStored`. The pause must report that, not
/// call it stored and not call it skipped.
/// What: the stub answers the error text `VerifyNotStored` renders.
#[tokio::test]
async fn a_verify_record_failure_is_reported_as_stored_without_verify() {
    let tmp = tempfile::TempDir::new().unwrap();
    repo_on_branch(tmp.path());
    let message = "drawer 00000000-0000-0000-0000-000000000000 is stored, but its \
                   verify command was not: kg write failed";
    let (daemon, _calls) = recording_daemon(Err((-32603, message.to_string()))).await;
    let memory = ResumeSlotHost {
        socket: daemon.socket().to_path_buf(),
        palace: Some("p".to_string()),
    };

    let result = pause(tmp.path(), &memory).await;

    assert_eq!(
        result["resume_slot"],
        json!("stored_without_verify:verify_not_stored"),
        "{result}"
    );
}

/// The PR numbers a pause's free text names, by the module's rule.
fn prs_of(summary: &str, rest: &[&str]) -> Vec<u64> {
    let completed: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
    super::pr_numbers(&trusty_common::catchup::pause::PauseSnapshotInput {
        session_id: "s",
        summary,
        completed: &completed,
        in_progress: &[],
        next_steps: &[],
        tmux_window: None,
    })
}

#[test]
fn pr_numbers_come_from_pr_mentions_only() {
    assert_eq!(
        prs_of(
            "PR #12, pr 7, PR#5, PRs #40 and https://github.com/o/r/pull/9",
            &["PR #12 again", "issue #9390 is not a PR"],
        ),
        vec![12, 7, 5, 40, 9]
    );
    for none in ["prune #3", "approve 12", "PR #12abc", "PR #0", "spr 4"] {
        assert_eq!(prs_of(none, &[]), Vec::<u64>::new(), "{none:?}");
    }
}

#[test]
fn an_unsafe_branch_gets_no_command() {
    for safe in ["main", "feat/9390-x", "release-1.2"] {
        assert!(super::is_safe_branch(safe), "{safe:?}");
    }
    for unsafe_name in ["HEAD", "-x", "a;rm -rf", "a$(id)", "a..b", "a b", ""] {
        assert!(!super::is_safe_branch(unsafe_name), "{unsafe_name:?}");
    }
    assert_eq!(super::verify_command(&[], None), None);
}

/// The marker matched against the socket error is the text PR B's error
/// renders, so a reworded message cannot silently turn into `memory_error`.
#[cfg(feature = "sm-memory")]
#[test]
fn the_verify_not_stored_marker_matches_the_error_text() {
    let missed = trusty_common::memory_core::retrieval::VerifyNotStored {
        drawer: uuid::Uuid::nil(),
        reason: "kg write failed".to_string(),
    };
    assert!(
        missed.to_string().contains(super::VERIFY_NOT_STORED_MARKER),
        "{missed}"
    );
}
