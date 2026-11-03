//! `tm pr merge` against code-critic verdicts, end to end (#10724).
//!
//! Every test drives [`merge::run`] through a scripted `gh`; nothing here
//! touches the network or a real PR.

use std::cell::RefCell;

use super::super::body::ATTRIBUTION_FOOTER;
use super::super::{EXIT_BLOCKED, EXIT_OK, GhRun, GhRunner, merge};
use crate::cli::PrMergeArgs;

/// The PR's current head commit.
const HEAD: &str = "0a400b509db40cb102929fa28341367b1ddc8840";
/// An earlier head the critic reviewed before a later push.
const OLD_HEAD: &str = "608d739081ed23fbc8703847ff08d17adeaeaa74";
/// The exact argv of the verdict read.
const VERDICT_READ: &str = "pr view 42 --repo o/r --json headRefOid,comments,reviews";

/// A fake `gh`: the first route whose needle the joined argv contains answers.
struct CriticGh {
    routes: Vec<(String, GhRun)>,
    seen: RefCell<Vec<String>>,
}

impl CriticGh {
    /// A clean, open PR with a passing check, plus `verdicts` as the answer to
    /// the verdict read.
    fn new(verdicts: serde_json::Value) -> Self {
        Self::with_verdict_read(GhRun {
            success: true,
            stdout: verdicts.to_string(),
            stderr: String::new(),
        })
    }

    fn with_verdict_read(read: GhRun) -> Self {
        let view = serde_json::json!({"number": 42, "title": "fix: x",
            "body": ATTRIBUTION_FOOTER, "state": "OPEN", "headRefName": "fix/x",
            "baseRefName": "main", "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"__typename": "CheckRun", "name": "CI gate",
                "status": "COMPLETED", "conclusion": "SUCCESS"}]});
        let ok = |stdout: &str| GhRun {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        Self {
            routes: vec![
                (VERDICT_READ.to_string(), read),
                ("pr view".to_string(), ok(&view.to_string())),
                ("pr merge".to_string(), ok("")),
            ],
            seen: RefCell::new(Vec::new()),
        }
    }

    fn merged(&self) -> bool {
        self.seen.borrow().iter().any(|a| a.starts_with("pr merge"))
    }
}

impl GhRunner for CriticGh {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        let joined = args.join(" ");
        self.seen.borrow_mut().push(joined.clone());
        let (_, run) = self
            .routes
            .iter()
            .find(|(needle, _)| joined.contains(needle.as_str()))
            .ok_or_else(|| anyhow::anyhow!("unexpected gh call: {joined}"))?;
        Ok(run.clone())
    }
}

/// `tm pr merge 42 --repo o/r`.
fn args() -> PrMergeArgs {
    PrMergeArgs {
        pr: 42,
        auto: false,
        no_delete_branch: false,
        no_cleanup: false,
        allow_failing: Vec::new(),
        allow_no_checks: false,
        repo: Some("o/r".to_string()),
    }
}

/// A COMMENT-type review on `commit` whose body opens with `verdict_line`.
fn review(verdict_line: &str, commit: &str, at: &str) -> serde_json::Value {
    serde_json::json!({"id": format!("PRR_{at}"), "author": {"login": "critic"},
        "state": "COMMENTED", "submittedAt": at, "commit": {"oid": commit},
        "body": format!("{verdict_line}\n\n## Findings\n\n| CRITICAL | a.rs | 1 |")})
}

/// A plain PR comment with `body`.
fn comment(body: &str, at: &str) -> serde_json::Value {
    serde_json::json!({"author": {"login": "critic"}, "createdAt": at, "body": body,
        "url": format!("https://github.com/o/r/pull/42#issuecomment-{at}")})
}

/// The verdict-read payload.
fn verdicts(comments: &[serde_json::Value], reviews: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({"headRefOid": HEAD, "comments": comments, "reviews": reviews})
}

/// FAILS BEFORE #10724: a BLOCK posted as a review on the head merged anyway.
#[test]
fn pr_10724_a_block_pinned_to_the_head_refuses() {
    let gh = CriticGh::new(verdicts(
        &[],
        &[review("## Verdict: BLOCK", HEAD, "2026-11-01T10:00:00Z")],
    ));
    assert_eq!(merge::run(&gh, &args()).expect("runs"), EXIT_BLOCKED);
    assert!(!gh.merged(), "{:?}", gh.seen.borrow());
}

/// A BLOCK on an older head is not a hold; it is a warning (#10724).
#[test]
fn pr_10724_a_block_on_an_old_head_does_not_refuse() {
    let gh = CriticGh::new(verdicts(
        &[],
        &[review(
            "## Verdict: BLOCK",
            OLD_HEAD,
            "2026-11-01T10:00:00Z",
        )],
    ));
    assert_eq!(merge::run(&gh, &args()).expect("runs"), EXIT_OK);
    assert!(gh.merged(), "{:?}", gh.seen.borrow());
}

/// A later APPROVE on the same head resolves the BLOCK (#10724).
#[test]
fn pr_10724_a_later_approve_on_the_head_resolves_the_block() {
    let gh = CriticGh::new(verdicts(
        &[],
        &[
            review("## Verdict: BLOCK", HEAD, "2026-11-01T10:00:00Z"),
            review("## Verdict: APPROVE", HEAD, "2026-11-01T11:00:00Z"),
        ],
    ));
    assert_eq!(merge::run(&gh, &args()).expect("runs"), EXIT_OK);
    assert!(gh.merged(), "{:?}", gh.seen.borrow());
}

/// FAILS BEFORE #10724: a BLOCK comment naming no commit fails closed.
#[test]
fn pr_10724_a_block_with_no_sha_refuses() {
    let gh = CriticGh::new(verdicts(
        &[comment(
            "## Verdict: BLOCK\n\nfindings",
            "2026-11-01T10:00:00Z",
        )],
        &[],
    ));
    assert_eq!(merge::run(&gh, &args()).expect("runs"), EXIT_BLOCKED);
    assert!(!gh.merged(), "{:?}", gh.seen.borrow());
}

/// Prose that mentions BLOCK is not a verdict line (#10724).
#[test]
fn pr_10724_prose_mentioning_block_is_ignored() {
    let gh = CriticGh::new(verdicts(
        &[comment(
            "trusty-review said verdict BLOCK earlier; Verdict: BLOCK was refuted. \
             pinned to 0a400b509d",
            "2026-11-01T10:00:00Z",
        )],
        &[],
    ));
    assert_eq!(merge::run(&gh, &args()).expect("runs"), EXIT_OK);
    assert!(gh.merged(), "{:?}", gh.seen.borrow());
}

/// FAILS BEFORE #10724: an unreadable verdict set never merges silently.
#[test]
fn pr_10724_a_verdict_read_failure_refuses() {
    let gh = CriticGh::with_verdict_read(GhRun {
        success: false,
        stdout: String::new(),
        stderr: "HTTP 502: Bad Gateway".to_string(),
    });
    assert_eq!(merge::run(&gh, &args()).expect("runs"), EXIT_BLOCKED);
    assert!(!gh.merged(), "{:?}", gh.seen.borrow());
}
