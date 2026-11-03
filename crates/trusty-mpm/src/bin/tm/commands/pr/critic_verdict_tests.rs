//! Unit tests for the code-critic verdict gate (#10724).

use super::super::{GhRun, GhRunner, argv};
use super::*;

const HEAD: &str = "0a400b509db40cb102929fa28341367b1ddc8840";
const OLD_HEAD: &str = "608d739081ed23fbc8703847ff08d17adeaeaa74";

fn review(line: &str, commit: &str, at: &str) -> Review {
    Review {
        id: format!("PRR_{at}"),
        author: Some(Author {
            login: "critic".to_string(),
        }),
        body: format!("{line}\n\n## Findings"),
        submitted_at: at.to_string(),
        commit: Some(CommitRef {
            oid: commit.to_string(),
        }),
    }
}

fn comment(body: &str, at: &str) -> Comment {
    Comment {
        body: body.to_string(),
        created_at: at.to_string(),
        url: format!("https://github.com/o/r/pull/42#issuecomment-{at}"),
    }
}

fn view(comments: Vec<Comment>, reviews: Vec<Review>) -> VerdictView {
    VerdictView {
        head: HEAD.to_string(),
        comments,
        reviews,
    }
}

#[test]
fn pr_10724_only_the_canonical_heading_is_a_verdict() {
    for prose in [
        "verdict BLOCK from trusty-review",
        "Verdict: BLOCK, pinned to 0a400b509d",
        "### Verdict: BLOCK",
        "## Verdict: Block",
        "## Verdict: BLOCKED",
        "## Verdict: <APPROVE|WARN|BLOCK>",
        "```\n## Verdict: BLOCK\n```",
    ] {
        assert_eq!(parse_verdict_line(prose), None, "{prose:?}");
    }
    assert_eq!(
        parse_verdict_line("intro\n  ## Verdict: WARN\n"),
        Some((Verdict::Warn, None))
    );
}

#[test]
fn pr_10724_a_pin_clause_is_parsed_from_the_verdict_line() {
    assert_eq!(
        parse_verdict_line("## Verdict: BLOCK, pinned to `0A400B509D`"),
        Some((Verdict::Block, Some("0a400b509d".to_string())))
    );
    // Too short to be a sha: no pin, so the BLOCK fails closed.
    assert_eq!(
        parse_verdict_line("## Verdict: BLOCK, pinned to abc"),
        Some((Verdict::Block, None))
    );
}

#[test]
fn pr_10724_the_head_block_refusal_names_source_and_sha() {
    let a = assess(&view(
        vec![],
        vec![review("## Verdict: BLOCK", HEAD, "2026-11-01T10:00:00Z")],
    ));
    let r = a.refusal.expect("refused");
    assert!(r.contains(HEAD), "{r}");
    assert!(
        r.contains("review PRR_2026-11-01T10:00:00Z by critic"),
        "{r}"
    );
}

#[test]
fn pr_10724_an_old_head_block_warns_that_no_verdict_covers_the_head() {
    let a = assess(&view(
        vec![],
        vec![review(
            "## Verdict: BLOCK",
            OLD_HEAD,
            "2026-11-01T10:00:00Z",
        )],
    ));
    assert_eq!(a.refusal, None);
    let w = a.warning.expect("warned");
    assert!(w.contains("no code-critic verdict covers the head"), "{w}");
    assert!(w.contains(HEAD) && w.contains(OLD_HEAD), "{w}");
}

#[test]
fn pr_10724_a_later_warn_resolves_a_head_block() {
    // A comment pinned by a short sha, after a review BLOCK on the head.
    let a = assess(&view(
        vec![comment(
            "## Verdict: WARN, pinned to 0a400b509d",
            "2026-11-01T11:00:00Z",
        )],
        vec![review("## Verdict: BLOCK", HEAD, "2026-11-01T10:00:00Z")],
    ));
    assert_eq!(a, Assessment::default());
}

#[test]
fn pr_10724_a_head_block_after_an_approve_refuses() {
    let a = assess(&view(
        vec![],
        vec![
            review("## Verdict: APPROVE", HEAD, "2026-11-01T10:00:00Z"),
            review("## Verdict: BLOCK", HEAD, "2026-11-01T11:00:00Z"),
        ],
    ));
    assert!(a.refusal.is_some(), "{a:?}");
}

#[test]
fn pr_10724_an_unpinned_block_names_the_fix() {
    let a = assess(&view(
        vec![comment("## Verdict: BLOCK", "2026-11-01T10:00:00Z")],
        vec![],
    ));
    let r = a.refusal.expect("refused");
    assert!(r.contains("names no commit sha"), "{r}");
    assert!(r.contains("issuecomment-2026-11-01T10:00:00Z"), "{r}");
    assert!(
        r.contains(&format!("## Verdict: APPROVE, pinned to {HEAD}")),
        "{r}"
    );
}

#[test]
fn pr_10724_an_unpinned_block_is_resolved_by_a_later_head_approve() {
    let a = assess(&view(
        vec![comment("## Verdict: BLOCK", "2026-11-01T10:00:00Z")],
        vec![review("## Verdict: APPROVE", HEAD, "2026-11-01T11:00:00Z")],
    ));
    assert_eq!(a, Assessment::default());
}

#[test]
fn pr_10724_no_verdict_is_neither_refusal_nor_warning() {
    let a = assess(&view(vec![comment("LGTM", "2026-11-01T10:00:00Z")], vec![]));
    assert_eq!(a, Assessment::default());
}

#[test]
fn pr_10724_verdicts_without_a_head_sha_refuse() {
    let mut v = view(
        vec![],
        vec![review("## Verdict: APPROVE", HEAD, "2026-11-01T10:00:00Z")],
    );
    v.head = String::new();
    assert!(assess(&v).refusal.is_some());
}

struct OneShot(GhRun);

impl GhRunner for OneShot {
    fn run(&self, _args: &[String]) -> anyhow::Result<GhRun> {
        Ok(self.0.clone())
    }
}

#[test]
fn pr_10724_unparseable_verdict_json_is_an_error() {
    let gh = OneShot(GhRun {
        success: true,
        stdout: "<html>rate limited</html>".to_string(),
        stderr: String::new(),
    });
    let err = read(&gh, argv(&["pr", "view", "42"])).expect_err("must not parse");
    assert!(format!("{err:#}").contains("cannot parse"), "{err:#}");
}
