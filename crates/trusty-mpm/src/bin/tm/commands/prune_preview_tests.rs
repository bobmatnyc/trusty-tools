//! Tests for the prune-worktrees preview, scope echo and `--force` two-step
//! (#8782). Every daemon here is a loopback stub; every repository a temp dir.

use serde_json::{Value, json};

use super::{check_scope_echo, planned_paths, preview_lines, project_root_from};
use crate::cli::{Cli, Command, SessionAction};
use crate::commands::managed_merged_prs::session_prune_worktrees;
use clap::Parser;

/// A reply as a scoped daemon returns it: one orphan, one merged-PR reclaim,
/// one unknown, all in `/r/a`, plus one reclaim in `/r/b`.
fn reply(project: Option<&str>) -> Value {
    json!({
        "scope": { "project_root": project, "only_paths": null },
        "paths": ["/r/a/.worktrees/o"],
        "orphan_rows": [
            { "path": "/r/a/.worktrees/o", "project": "/r/a", "reason": "orphaned — ended" }
        ],
        "merged_prs": {
            "reclaimable_paths": [
                { "path": "/r/a/.claude/worktrees/m", "project": "/r/a", "reason": "reclaimable — landing evidence is PR #7" },
                { "path": "/r/b/.claude/worktrees/n", "project": "/r/b", "reason": "reclaimable — landing evidence is PR #9" }
            ],
            "unknown_paths": [
                { "path": "/r/a/.claude/worktrees/u", "project": "/r/a", "reason": "refused at gate 5 — the pull-request lookup failed: no `origin` remote" }
            ]
        }
    })
}

#[test]
fn cli_prune_worktrees_all_projects_is_opt_in() {
    let parse = |args: &[&str]| match Cli::try_parse_from(args).expect("parses").command {
        Some(Command::Session {
            action: SessionAction::PruneWorktrees { all_projects, .. },
        }) => all_projects,
        other => panic!("expected session prune-worktrees, got {other:?}"),
    };
    assert!(!parse(&[
        "tm",
        "session",
        "prune-worktrees",
        "--force",
        "--merged-prs"
    ]));
    assert!(parse(&[
        "tm",
        "session",
        "prune-worktrees",
        "--all-projects"
    ]));
}

#[test]
fn planned_paths_is_the_union_of_both_passes() {
    assert_eq!(
        planned_paths(&reply(None)),
        vec![
            "/r/a/.claude/worktrees/m",
            "/r/a/.worktrees/o",
            "/r/b/.claude/worktrees/n"
        ],
        "unknown rows are never planned"
    );
}

#[test]
fn preview_lines_group_every_path_by_project_with_a_count() {
    let lines = preview_lines(&reply(Some("/r/a")));
    let text = lines.join("\n");
    assert!(lines[0].contains("scope: project /r/a"), "{text}");
    assert!(
        text.contains("project /r/a: 2 to remove, 1 unknown (kept)"),
        "{text}"
    );
    assert!(
        text.contains("project /r/b: 1 to remove, 0 unknown (kept)"),
        "{text}"
    );
    assert!(
        text.contains(
            "  remove   /r/a/.claude/worktrees/m — reclaimable — landing evidence is PR #7"
        ),
        "{text}"
    );
    assert!(
        text.contains("  unknown  /r/a/.claude/worktrees/u — refused at gate 5"),
        "{text}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("total: 3 worktree(s) to remove, 1 kept as unknown, across 2 project(s)")
    );
}

#[test]
fn preview_lines_report_an_empty_preview() {
    let lines = preview_lines(&json!({ "scope": { "project_root": null } }));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("all registered projects"));
    assert!(lines[1].starts_with("total: 0 worktree(s) to remove"));
}

#[test]
fn a_reply_without_the_scope_echo_is_refused() {
    let stale = json!({ "paths": [] });
    let err = check_scope_echo(&stale, Some("/r/a"), true).expect_err("stale daemon");
    assert!(err.to_string().contains("predates #8782"), "{err}");
    // An all-projects preview from an old daemon is what it always was.
    assert!(check_scope_echo(&stale, None, false).is_ok());
}

#[test]
fn a_reply_scoped_to_another_project_is_refused() {
    assert!(check_scope_echo(&reply(Some("/r/a")), Some("/r/a"), true).is_ok());
    assert!(check_scope_echo(&reply(Some("/r/b")), Some("/r/a"), true).is_err());
    assert!(check_scope_echo(&reply(None), Some("/r/a"), true).is_err());
    assert!(check_scope_echo(&reply(Some("/r/a")), None, true).is_err());
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn project_root_from_a_linked_worktree_is_its_main_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = std::fs::canonicalize(tmp.path())
        .expect("canonical")
        .join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    git(&repo, &["init", "-q", "--initial-branch=main"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=ci@test.invalid",
            "-c",
            "user.name=CI",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ],
    );
    let wt = repo.join(".claude").join("worktrees").join("agent-8782");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt/8782",
            wt.to_str().expect("utf8"),
        ],
    );
    let root = project_root_from(&wt).expect("inside a repository");
    assert_eq!(root, repo.to_string_lossy());
}

#[test]
fn project_root_from_outside_a_repository_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let err = project_root_from(tmp.path()).expect_err("no repository, no project");
    assert!(err.to_string().contains("--all-projects"), "{err}");
}

/// A stub daemon answering each prune POST with the next of `replies`, then
/// waiting briefly for one more; returns every request body it received.
async fn stub_daemon(replies: Vec<Value>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let handle = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for reply in replies {
            let wait = std::time::Duration::from_millis(if bodies.is_empty() { 5000 } else { 500 });
            let Ok(Ok((mut sock, _))) = tokio::time::timeout(wait, listener.accept()).await else {
                break;
            };
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            let (head_end, len) = loop {
                let n = sock.read(&mut buf).await.expect("read");
                assert!(n > 0, "connection closed mid-request");
                seen.extend_from_slice(&buf[..n]);
                if let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&seen[..i]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .expect("content-length");
                    break (i + 4, len);
                }
            };
            while seen.len() < head_end + len {
                let n = sock.read(&mut buf).await.expect("read body");
                seen.extend_from_slice(&buf[..n]);
            }
            bodies.push(serde_json::from_slice(&seen[head_end..head_end + len]).expect("json"));
            let payload = reply.to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{payload}",
                payload.len()
            );
            sock.write_all(resp.as_bytes()).await.expect("write");
        }
        bodies
    });
    (url, handle)
}

/// 🔴 #8782: `--force` previews, then sends exactly the preview's paths as
/// `only_paths`, scoped to the same project.
#[tokio::test]
async fn force_sends_only_the_previewed_paths() {
    let (url, server) = stub_daemon(vec![reply(Some("/r/a")), reply(Some("/r/a"))]).await;
    let client = reqwest::Client::new();
    let outcome =
        session_prune_worktrees(&client, &url, false, false, true, None, Some("/r/a".into())).await;
    let bodies = server.await.expect("stub");
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert_eq!(bodies[0]["dry_run"], true, "the first call is the preview");
    assert!(bodies[0]["only_paths"].is_null());
    assert_eq!(bodies[1]["dry_run"], false);
    assert_eq!(bodies[1]["project_root"], "/r/a");
    assert_eq!(bodies[1]["only_paths"], json!(planned_paths(&reply(None))));
}

/// 🔴 #8782: a daemon that does not echo the scope gets no destructive call.
#[tokio::test]
async fn force_sends_nothing_after_a_reply_without_the_scope_echo() {
    let stale = json!({ "dry_run": true, "paths": ["/r/a/.worktrees/o"] });
    let (url, server) = stub_daemon(vec![stale.clone(), stale]).await;
    let client = reqwest::Client::new();
    let outcome = session_prune_worktrees(
        &client,
        &url,
        false,
        false,
        false,
        None,
        Some("/r/a".into()),
    )
    .await;
    let bodies = server.await.expect("stub");
    assert!(outcome.is_err(), "a stale daemon's reply was accepted");
    assert_eq!(
        bodies.len(),
        1,
        "a destructive call followed the refusal: {bodies:?}"
    );
}
