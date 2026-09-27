//! Tests for the prune-worktrees preview, scope echo and `--force` two-step
//! (#8782). Every daemon here is a loopback stub; every repository a temp dir.

use serde_json::{Value, json};

use super::{
    PlannedPaths, check_allowlist_echo, check_scope_echo, planned_paths, preview_lines,
    project_root_from, removed_lines,
};
use crate::cli::{Cli, Command, SessionAction};
use crate::commands::managed_merged_prs::{prune_worktrees_from, session_prune_worktrees};
use clap::Parser;

/// A reply as a scoped daemon returns it: one orphan, one merged-PR reclaim,
/// one unknown, all in `/r/a`, plus one reclaim in `/r/b`.
fn reply(project: Option<&str>) -> Value {
    json!({
        "scope": {
            "project_root": project,
            "project_known": true,
            "only_orphan_paths": null,
            "only_merged_paths": null,
            "only_discard_paths": null
        },
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
fn planned_paths_keeps_each_pass_to_its_own_rows() {
    let planned = planned_paths(&reply(None));
    assert_eq!(planned.orphan, vec!["/r/a/.worktrees/o"]);
    assert_eq!(
        planned.merged,
        vec!["/r/a/.claude/worktrees/m", "/r/b/.claude/worktrees/n"],
        "unknown rows are never planned"
    );
    assert!(planned.discard.is_empty(), "no row discards unsaved work");
    assert!(planned_paths(&json!({})).is_empty());
    // #8782: only a row marked as discarding unsaved work is in the discard list.
    let rows = json!({ "orphan_rows": [
        { "path": "/r/a/.worktrees/d", "discards_unsaved_work": true },
        { "path": "/r/a/.worktrees/c", "discards_unsaved_work": false },
        { "path": "/r/a/.worktrees/old" }
    ] });
    assert_eq!(planned_paths(&rows).discard, vec!["/r/a/.worktrees/d"]);
}

/// A force reply echoing the sizes of the allowlists `planned` sent.
fn force_reply(project: Option<&str>, planned: &PlannedPaths) -> Value {
    let mut body = reply(project);
    body["scope"]["only_orphan_paths"] = json!(planned.orphan.len());
    body["scope"]["only_merged_paths"] = json!(planned.merged.len());
    body["scope"]["only_discard_paths"] = json!(planned.discard.len());
    body
}

/// 🔴 #8782: a `--force` preview from a daemon that drops an allowlist key is
/// refused before any removal is sent; null values are accepted.
#[test]
fn a_force_preview_without_the_allowlist_keys_is_refused() {
    assert!(check_allowlist_echo(&reply(Some("/r/a")), None).is_ok());
    for key in [
        "only_orphan_paths",
        "only_merged_paths",
        "only_discard_paths",
    ] {
        let mut body = reply(Some("/r/a"));
        body["scope"].as_object_mut().expect("echo").remove(key);
        let err = check_allowlist_echo(&body, None).expect_err(key);
        assert!(
            err.to_string().contains(key) && err.to_string().contains("nothing destructive"),
            "{err}"
        );
    }
}

/// 🔴 #8782: a force reply whose echoed allowlist sizes differ from what was
/// sent is an error.
#[test]
fn a_force_reply_whose_allowlist_sizes_differ_is_refused() {
    let planned = planned_paths(&reply(None));
    let good = force_reply(Some("/r/a"), &planned);
    assert!(check_allowlist_echo(&good, Some(&planned)).is_ok());
    let mut unbounded = good.clone();
    unbounded["scope"]["only_merged_paths"] = Value::Null;
    assert!(check_allowlist_echo(&unbounded, Some(&planned)).is_err());
    let mut wider = good;
    wider["scope"]["only_orphan_paths"] = json!(planned.orphan.len() + 1);
    let err = check_allowlist_echo(&wider, Some(&planned)).expect_err("size differs");
    assert!(err.to_string().contains("only_orphan_paths"), "{err}");
}

/// 🔴 #8782: a removal reply names every discard of unsaved work.
#[test]
fn removed_lines_name_every_discard() {
    let body = json!({
        "paths": ["/r/a/.worktrees/d"],
        "orphan_rows": [{ "path": "/r/a/.worktrees/d", "project": "/r/a",
            "reason": "orphaned — it holds unsaved work (1 file) — discarded (--discard-dirty)" }]
    });
    assert_eq!(
        removed_lines(&body),
        vec![
            "removed  /r/a/.worktrees/d — orphaned — it holds unsaved work (1 file) — \
             discarded (--discard-dirty)"
        ]
    );
    let old = json!({ "paths": ["/r/a/.worktrees/o"] });
    assert_eq!(removed_lines(&old), vec!["/r/a/.worktrees/o"]);
}

/// #8782: a scoped run from a checkout the daemon does not scan says so,
/// rather than printing a bare `total: 0`.
#[test]
fn preview_lines_say_when_the_daemon_does_not_scan_this_checkout() {
    let unknown = json!({ "scope": { "project_root": "/r/z", "project_known": false } });
    let text = preview_lines(&unknown).join("\n");
    assert!(
        text.contains("this checkout is not registered with the daemon"),
        "{text}"
    );
    let known = preview_lines(&reply(Some("/r/a"))).join("\n");
    assert!(!known.contains("not registered"), "{known}");
}

/// #8782: an older daemon's `--all-projects` preview has no rows, so the CLI
/// prints that reply's orphan paths and says the per-project view is missing.
#[test]
fn preview_lines_fall_back_to_the_path_list_of_an_older_daemon() {
    let stale = json!({ "dry_run": true, "paths": ["/r/a/.worktrees/o"] });
    let lines = preview_lines(&stale);
    assert!(
        lines[0].contains("per-project preview is unavailable"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("/r/a/.worktrees/o")),
        "{lines:?}"
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
/// per-pass allowlists, scoped to the same project.
#[tokio::test]
async fn force_sends_only_the_previewed_paths() {
    let planned = planned_paths(&reply(None));
    let (url, server) = stub_daemon(vec![
        reply(Some("/r/a")),
        force_reply(Some("/r/a"), &planned),
    ])
    .await;
    let client = reqwest::Client::new();
    let outcome =
        session_prune_worktrees(&client, &url, false, false, true, None, Some("/r/a".into())).await;
    let bodies = server.await.expect("stub");
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert_eq!(bodies[0]["dry_run"], true, "the first call is the preview");
    assert!(bodies[0]["only_orphan_paths"].is_null());
    assert!(bodies[0]["only_merged_paths"].is_null());
    assert_eq!(bodies[1]["dry_run"], false);
    assert_eq!(bodies[1]["project_root"], "/r/a");
    assert_eq!(bodies[1]["only_orphan_paths"], json!(planned.orphan));
    assert_eq!(bodies[1]["only_merged_paths"], json!(planned.merged));
    assert_eq!(bodies[1]["only_discard_paths"], json!(planned.discard));
}

/// 🔴 #8782: a `--force` run whose preview echo lacks the allowlist keys sends
/// no removal request.
///
/// Fails when `session_prune_worktrees` skips `check_allowlist_echo` on the
/// preview: the daemon, which would ignore the allowlists, gets a second POST.
#[tokio::test]
async fn force_sends_nothing_after_a_preview_without_the_allowlist_keys() {
    let mut older = reply(Some("/r/a"));
    let echo = older["scope"].as_object_mut().expect("echo");
    echo.remove("only_orphan_paths");
    echo.remove("only_merged_paths");
    echo.remove("only_discard_paths");
    let (url, server) = stub_daemon(vec![older.clone(), older]).await;
    let client = reqwest::Client::new();
    let outcome =
        session_prune_worktrees(&client, &url, false, false, true, None, Some("/r/a".into())).await;
    let bodies = server.await.expect("stub");
    let err = outcome.expect_err("an echo without the allowlist keys was accepted");
    assert!(err.to_string().contains("only_orphan_paths"), "{err}");
    assert_eq!(bodies.len(), 1, "a removal request was sent: {bodies:?}");
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

/// 🔴 #8782: an `--all-projects --force` run gets no destructive call from a
/// daemon that does not echo the scope, because it would ignore the allowlists.
///
/// Fails when `session_prune_worktrees` requires the echo only for a scoped run
/// (`project.is_some()` without `|| !dry_run`): the stale preview is accepted
/// and a second, destructive POST follows.
#[tokio::test]
async fn force_with_all_projects_sends_nothing_after_a_reply_without_the_scope_echo() {
    let stale = json!({ "dry_run": true, "paths": ["/r/a/.worktrees/o"] });
    let (url, server) = stub_daemon(vec![stale.clone(), stale]).await;
    let client = reqwest::Client::new();
    let outcome = session_prune_worktrees(&client, &url, false, false, false, None, None).await;
    let bodies = server.await.expect("stub");
    assert!(outcome.is_err(), "a stale daemon's reply was accepted");
    assert_eq!(
        bodies.len(),
        1,
        "a destructive call followed the refusal: {bodies:?}"
    );
    assert!(bodies[0]["project_root"].is_null(), "{bodies:?}");
}

/// 🔴 #8782: outside a git repository the dispatch posts nothing at all.
///
/// Fails when `prune_worktrees_from` reads an unresolvable project as "no
/// project" (`.ok()` for `?`): the run widens to every project and posts.
#[tokio::test]
async fn prune_worktrees_outside_a_repository_posts_nothing() {
    let (url, server) = stub_daemon(vec![reply(Some("/r/a"))]).await;
    let client = reqwest::Client::new();
    // #8782: hermetic, so a `TMPDIR` inside a git repository cannot make this
    // directory a checkout and mask the error arm.
    let outside = crate::test_support::hermetic_temp_dir();
    let outcome = prune_worktrees_from(
        &client,
        &url,
        outside.path(),
        true,
        false,
        true,
        false,
        None,
    )
    .await;
    let bodies = server.await.expect("stub");
    let err = outcome.expect_err("no repository, no project, no request");
    assert!(err.to_string().contains("--all-projects"), "{err}");
    assert!(bodies.is_empty(), "a request was posted: {bodies:?}");
}

// Moved from `tests_behavior_d_tests.rs` by #8782 (test-file SLOC cap).
/// #2919: the merged-pull-request reclaim pass requires its own explicit flag.
///
/// Why: it is the only reclaim path that acts on GitHub state, so an operator
/// clearing stale directories must opt into it deliberately rather than
/// inheriting it from `--force`. Pinning it as a third independent flag is what
/// keeps anything automatic from ever reaching a merged-PR deletion.
#[test]
fn cli_prune_worktrees_merged_prs_is_opt_in() {
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "session",
        "prune-worktrees",
        "--force",
        "--merged-prs",
    ])
    .unwrap();
    match cli.command.unwrap() {
        Command::Session {
            action:
                SessionAction::PruneWorktrees {
                    force,
                    discard_dirty,
                    merged_prs,
                    ..
                },
        } => {
            assert!(force);
            assert!(merged_prs, "--merged-prs must set merged_prs=true");
            assert!(
                !discard_dirty,
                "#2919: --merged-prs must NOT imply discarding uncommitted work"
            );
        }
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
}
