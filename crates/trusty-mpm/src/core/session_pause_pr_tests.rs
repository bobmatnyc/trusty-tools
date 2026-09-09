//! Tests for [`super::publish_pause_snapshot`] (#7282).
//!
//! Every test drives [`FakeVcs`], so none of them needs a git repository, a
//! network, or a `gh` install — which is what makes the not-on-`main` refusal
//! and the post-commit push failure testable at all.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use chrono::TimeZone as _;

use super::*;

/// One recorded subprocess invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    /// `"git"` or `"tm"`.
    program: &'static str,
    /// The argv, without the program.
    args: Vec<String>,
    /// The `GIT_INDEX_FILE` overlay, when one was set.
    index_file: Option<PathBuf>,
}

/// A scripted [`PauseVcs`] that records every call.
///
/// Why: the publish sequence's contract is the exact set of commands it runs —
/// specifically that no `git add` ever appears and that only allowlisted paths
/// enter `update-index`. Recording is how that is asserted.
/// What: responses are matched by argv prefix, longest match first and — at
/// equal length — last one scripted, so one test can override one step of the
/// happy path. Anything unmatched succeeds with empty output.
struct FakeVcs {
    /// `(argv prefix, response)` pairs.
    script: Vec<(Vec<String>, CmdOut)>,
    /// Everything the publish ran, in order.
    calls: RefCell<Vec<Call>>,
}

impl FakeVcs {
    fn new() -> Self {
        Self {
            script: Vec::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Script a success with `stdout` for any argv starting with `prefix`.
    fn ok(mut self, prefix: &[&str], stdout: &str) -> Self {
        self.script.push((
            prefix.iter().map(|s| (*s).to_string()).collect(),
            CmdOut {
                success: true,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        ));
        self
    }

    /// Script a failure with `stderr` for any argv starting with `prefix`.
    fn fail(mut self, prefix: &[&str], stderr: &str) -> Self {
        self.script.push((
            prefix.iter().map(|s| (*s).to_string()).collect(),
            CmdOut {
                success: false,
                stdout: String::new(),
                stderr: stderr.to_string(),
            },
        ));
        self
    }

    fn respond(&self, args: &[String]) -> CmdOut {
        let mut best: Option<&(Vec<String>, CmdOut)> = None;
        for entry in &self.script {
            if args.len() >= entry.0.len()
                && args[..entry.0.len()] == entry.0[..]
                && best.is_none_or(|b| entry.0.len() >= b.0.len())
            {
                best = Some(entry);
            }
        }
        best.map(|e| e.1.clone()).unwrap_or(CmdOut {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    /// The argv of every recorded `git` call, joined for substring assertions.
    fn git_argv(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.program == "git")
            .map(|c| c.args.join(" "))
            .collect()
    }
}

impl PauseVcs for FakeVcs {
    fn git(
        &self,
        _repo: &Path,
        args: &[String],
        index_file: Option<&Path>,
    ) -> anyhow::Result<CmdOut> {
        self.calls.borrow_mut().push(Call {
            program: "git",
            args: args.to_vec(),
            index_file: index_file.map(Path::to_path_buf),
        });
        Ok(self.respond(args))
    }

    fn tm(&self, _repo: &Path, args: &[String]) -> anyhow::Result<CmdOut> {
        self.calls.borrow_mut().push(Call {
            program: "tm",
            args: args.to_vec(),
            index_file: None,
        });
        Ok(self.respond(args))
    }
}

/// A driver scripted for the whole happy path.
fn happy() -> FakeVcs {
    FakeVcs::new()
        .ok(&["rev-parse", "--abbrev-ref", "HEAD"], "main\n")
        // exit 1 from `check-ignore` = the path is NOT ignored = tracked here.
        .fail(&["check-ignore"], "")
        .ok(&["rev-parse", "origin/main"], "base000\n")
        .ok(&["rev-parse", "base000^{tree}"], "basetree0\n")
        .ok(&["hash-object"], "blob111\n")
        .ok(&["write-tree"], "newtree9\n")
        .ok(&["commit-tree"], "c0ffee1\n")
        .ok(
            &["pr", "open"],
            "opened PR #4242 — https://github.com/bobmatnyc/trusty-tools/pull/4242\n",
        )
        .ok(&["pr", "merge"], "auto-merge armed on #4242\n")
}

fn ts() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2026, 9, 9, 18, 30, 15)
        .unwrap()
}

fn request<'a>(dir: &'a Path, paths: Vec<String>) -> PublishRequest<'a> {
    PublishRequest {
        repo: dir,
        session_id: "trusty-tools-95",
        timestamp: ts(),
        paths,
        body_dir: dir,
    }
}

fn snapshot_paths() -> Vec<String> {
    vec![
        ".trusty-mpm/sessions/abc/session-20260909-183015.md".to_string(),
        ".trusty-mpm/sessions/sessions-log.jsonl".to_string(),
    ]
}

/// Why: the defect was a commit on whatever branch the checkout was on. The
/// branch name is the receipt that a pause got its own branch off `origin/main`.
/// Test target: `branch_name`, via the public entry point.
#[test]
fn publish_branch_name_carries_session_and_timestamp() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = happy();
    let out = publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths()))
        .unwrap()
        .expect("a changed tree publishes");

    assert_eq!(out.branch, "chore/sessions-trusty-tools-95-20260909-183015");
    assert_eq!(out.commit, "c0ffee1");
    assert_eq!(
        out.pr_url,
        "https://github.com/bobmatnyc/trusty-tools/pull/4242"
    );
    assert!(out.auto_merge_armed);

    // The commit is parented on `origin/main`, not on local HEAD.
    let argv = vcs.git_argv();
    assert!(
        argv.iter()
            .any(|a| a.starts_with("commit-tree newtree9 -p base000")),
        "{argv:?}"
    );
    // The branch ref is created and pushed without ever checking it out.
    assert!(
        argv.iter()
            .any(|a| a
                == "update-ref refs/heads/chore/sessions-trusty-tools-95-20260909-183015 c0ffee1"),
        "{argv:?}"
    );
    assert!(
        argv.iter()
            .any(|a| a.starts_with("push origin refs/heads/")),
        "{argv:?}"
    );
}

/// Why: the whole safety claim of this change is that a pause can never sweep
/// in a file it does not own. `git add` is the command that would, and the
/// scratch index is what keeps the shared one out of it.
/// Test target: the `update-index` / `read-tree` sequence.
#[test]
fn publish_commits_only_the_allowlisted_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = happy();
    publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap();

    let argv = vcs.git_argv();
    assert!(
        !argv.iter().any(|a| a.starts_with("add")),
        "a pause must never run `git add`: {argv:?}"
    );
    assert!(
        !argv
            .iter()
            .any(|a| a.contains("stash") || a.contains("checkout")),
        "a pause must not stash or check anything out: {argv:?}"
    );

    let staged: Vec<&String> = argv
        .iter()
        .filter(|a| a.starts_with("update-index"))
        .collect();
    assert_eq!(staged.len(), 2, "{argv:?}");
    assert_eq!(
        staged[0],
        "update-index --add --cacheinfo 100644,blob111,.trusty-mpm/sessions/abc/session-20260909-183015.md"
    );
    assert_eq!(
        staged[1],
        "update-index --add --cacheinfo 100644,blob111,.trusty-mpm/sessions/sessions-log.jsonl"
    );

    // Every index-mutating call uses the scratch index, never the shared one.
    for call in vcs.calls().iter().filter(|c| c.program == "git") {
        let verb = call.args.first().map(String::as_str).unwrap_or("");
        if matches!(verb, "read-tree" | "update-index" | "write-tree") {
            assert!(
                call.index_file.is_some(),
                "`{verb}` ran against the shared index"
            );
        }
    }
}

/// Why: a caller that could name any path would turn the pause into an
/// arbitrary-commit primitive, which is exactly what the allowlist forbids.
/// Test target: `allowlisted`.
#[test]
fn publish_rejects_a_path_outside_the_sessions_tree() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = happy();
    let paths = vec![
        ".trusty-mpm/sessions/abc/session-1.md".to_string(),
        "crates/trusty-mpm/src/lib.rs".to_string(),
    ];
    let err = publish_pause_snapshot(&vcs, &request(dir.path(), paths)).unwrap_err();

    assert!(
        matches!(
            &err,
            PublishError::Step {
                step: "allowlist",
                ..
            }
        ),
        "{err:?}"
    );
    assert!(
        err.to_string().contains("crates/trusty-mpm/src/lib.rs"),
        "{err}"
    );
    assert!(
        vcs.calls().is_empty(),
        "nothing may be spawned once the allowlist rejects a path"
    );
}

/// Why: publishing off a feature branch would put unrelated commits in the PR.
/// The refusal must be an error, not a warning — the snapshot file is already
/// written, and a silent skip is how the old behaviour went unnoticed.
/// Test target: the `REQUIRED_BRANCH` guard.
#[test]
fn publish_refuses_when_not_on_main() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = FakeVcs::new().ok(
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "fix/7282-something\n",
    );
    let err = publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap_err();

    assert_eq!(
        err,
        PublishError::NotOnMain("fix/7282-something".to_string())
    );
    assert!(
        err.to_string().contains("The snapshot file was written"),
        "{err}"
    );
    assert_eq!(
        vcs.calls().len(),
        1,
        "nothing beyond the branch probe may run: {:?}",
        vcs.calls()
    );
}

/// Why: when the push fails the commit already exists locally, and a person
/// needs to be told exactly where it is instead of re-running a pause that
/// would build a second one.
/// Test target: `git_at`'s commit attribution.
#[test]
fn push_failure_leaves_the_commit_and_names_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = happy().fail(&["push"], "remote rejected: no write access");
    let err = publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap_err();

    let PublishError::Step {
        step,
        branch,
        commit,
        ..
    } = &err
    else {
        panic!("{err:?}");
    };
    assert_eq!(*step, "push");
    assert_eq!(
        branch.as_deref(),
        Some("chore/sessions-trusty-tools-95-20260909-183015")
    );
    assert_eq!(commit.as_deref(), Some("c0ffee1"));
    let text = err.to_string();
    assert!(text.contains("remote rejected"), "{text}");
    assert!(text.contains("push it and open its PR by hand"), "{text}");

    // The local branch ref survives for a person to push.
    assert!(
        vcs.git_argv().iter().any(|a| a.starts_with("update-ref ")),
        "the branch must still have been created"
    );
    // `tm pr open` was never reached.
    assert!(
        !vcs.calls().iter().any(|c| c.program == "tm"),
        "{:?}",
        vcs.calls()
    );
}

/// Why: `tm pr open` failing after a successful push leaves a pushed branch
/// with no PR, which is a different repair than a failed push.
#[test]
fn pr_open_failure_reports_the_pushed_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = happy().fail(&["pr", "open"], "check failed: workstream label");
    let err = publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap_err();

    let PublishError::Step { step, commit, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(*step, "pr-open");
    assert_eq!(commit.as_deref(), Some("c0ffee1"));
    assert!(err.to_string().contains("workstream label"), "{err}");
}

/// Why: `tm pr open` is the workspace's only `gh pr create` site, and this
/// module must reach GitHub through it rather than growing a second one.
#[test]
fn publish_opens_the_pr_through_tm_pr_open() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = happy();
    publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap();

    let tm: Vec<Vec<String>> = vcs
        .calls()
        .into_iter()
        .filter(|c| c.program == "tm")
        .map(|c| c.args)
        .collect();
    assert_eq!(tm.len(), 2, "{tm:?}");
    assert_eq!(tm[0][0..2], ["pr".to_string(), "open".to_string()]);
    assert!(tm[0].contains(&"--docs-only".to_string()), "{:?}", tm[0]);
    assert!(tm[0].contains(&"--rung".to_string()), "{:?}", tm[0]);
    assert_eq!(
        tm[0][tm[0].iter().position(|a| a == "--title").unwrap() + 1],
        "chore(sessions): pause snapshot for trusty-tools-95 2026-09-09 18:30Z"
    );
    assert_eq!(
        tm[1],
        vec![
            "pr".to_string(),
            "merge".to_string(),
            "4242".to_string(),
            "--auto".to_string()
        ]
    );

    // The body file `tm pr open` was handed satisfies the seven-field contract.
    let body_arg = &tm[0][tm[0].iter().position(|a| a == "--body-file").unwrap() + 1];
    let body = std::fs::read_to_string(body_arg).unwrap();
    for heading in [
        "## Outcome",
        "## Changes",
        "## Risk",
        "## Tests",
        "## Baseline",
        "## Docs",
        "## Review",
    ] {
        assert!(body.contains(heading), "body is missing {heading}:\n{body}");
    }
    assert!(body.trim_end().ends_with(ATTRIBUTION_FOOTER), "{body}");
    assert!(
        !body.to_lowercase().contains("closes #"),
        "a session PR closes nothing: {body}"
    );
}

/// Why: a project that keeps `.trusty-mpm/sessions/` git-ignored has nothing to
/// publish, and turning that into a failed pause would break every such project.
#[test]
fn publish_skips_a_project_that_ignores_its_sessions() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = FakeVcs::new()
        .ok(&["rev-parse", "--abbrev-ref", "HEAD"], "main\n")
        // exit 0 from `check-ignore` = the path IS ignored.
        .ok(&["check-ignore"], "");
    let err = publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap_err();

    assert!(matches!(err, PublishError::NotTracked(_)), "{err:?}");
    assert!(
        !vcs.git_argv().iter().any(|a| a.starts_with("fetch")),
        "nothing may be fetched for an untracked sessions tree"
    );
}

/// Why: re-publishing an unchanged snapshot would open an empty PR on every
/// pause. Comparing the assembled tree against `origin/main`'s is what stops it.
#[test]
fn publish_is_a_noop_when_the_tree_is_unchanged() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = happy().ok(&["write-tree"], "basetree0\n");
    let out = publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap();

    assert!(out.is_none(), "{out:?}");
    assert!(
        !vcs.git_argv().iter().any(|a| a.starts_with("commit-tree")),
        "no commit may be created when nothing changed"
    );
    assert!(!vcs.calls().iter().any(|c| c.program == "tm"));
}

/// Why: `session_context_pause` also runs against project directories that are
/// not checkouts at all. Treating that as a failed pause would break every such
/// project — and did break seven existing tests before this arm was added.
#[test]
fn publish_skips_a_directory_that_is_not_a_git_repo() {
    let dir = tempfile::TempDir::new().unwrap();
    let vcs = FakeVcs::new().fail(
        &["rev-parse", "--abbrev-ref", "HEAD"],
        "fatal: not a git repository (or any of the parent directories): .git",
    );
    let err = publish_pause_snapshot(&vcs, &request(dir.path(), snapshot_paths())).unwrap_err();

    assert!(matches!(err, PublishError::NotAGitRepo(_)), "{err:?}");
    assert_eq!(vcs.calls().len(), 1, "{:?}", vcs.calls());
}

/// Why: a session id with slashes or spaces would produce an invalid ref.
#[test]
fn slug_bounds_and_sanitizes_the_session_id() {
    assert_eq!(super::slug("trusty-tools-95"), "trusty-tools-95");
    assert_eq!(super::slug("tmux window/1"), "tmux-window-1");
    assert_eq!(super::slug("///"), "session");
    assert_eq!(super::slug(&"x".repeat(80)).len(), 40);
}
