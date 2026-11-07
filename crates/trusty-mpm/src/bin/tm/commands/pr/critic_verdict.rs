//! `tm pr merge`'s code-critic verdict gate (#10724).
//!
//! Why: the critic posts APPROVE/WARN/BLOCK as a COMMENT-type review, never as
//! `CHANGES_REQUESTED` (GitHub refuses that event on a self-authored PR), so
//! `reviewDecision` never reflects a BLOCK and an unresolved BLOCK merged.
//!
//! What: [`read`] fetches the head sha with every comment and review in one
//! `gh pr view`; [`assess`] keeps the verdicts of trusted authors, pins each
//! to a commit, and refuses while the latest verdict pinned to the head is a
//! BLOCK, while a BLOCK that names no commit is not followed by an APPROVE or
//! WARN pinned to the head, or while a BLOCK on an earlier commit is not
//! followed by any APPROVE or WARN. An APPROVE or WARN that covers only older
//! heads is a warning.
//!
//! The verdict line is the `code-review-standards` Output Format heading,
//! `## Verdict: <APPROVE|WARN|BLOCK>`, at the start of a line outside a code
//! fence. APPROVE and WARN match case-sensitively, with optional leading `#`
//! marks and `**` bold; a BLOCK matches in any case and decoration, so an
//! unread BLOCK cannot let a merge through. A review is pinned to its own
//! `commit.oid`; a
//! `pinned to <sha>` clause on the verdict line overrides that, and is the only
//! pin a plain comment can carry.
//!
//! Test: `critic_verdict_tests.rs`, and `merge_critic_tests.rs` end to end.

use serde::Deserialize;

use super::GhRunner;

/// The verdict label (`code-review-standards`, Output Format).
const VERDICT_LABEL: &str = "Verdict:";

/// Markdown bold, tolerated around the label and the value (#10724).
const BOLD: &str = "**";

/// The clause that pins a verdict line to a commit.
const PIN_CLAUSE: &str = "pinned to ";

/// The `authorAssociation` roles whose verdicts count (#10796 review, H1).
const TRUSTED: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];

/// One code-critic verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approve,
    Warn,
    Block,
}

impl Verdict {
    /// The verdict word as the critic writes it.
    fn label(self) -> &'static str {
        match self {
            Verdict::Approve => "APPROVE",
            Verdict::Warn => "WARN",
            Verdict::Block => "BLOCK",
        }
    }
}

/// The `gh pr view --json headRefOid,comments,reviews` payload.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct VerdictView {
    /// The head commit the merge would land.
    #[serde(default, rename = "headRefOid")]
    pub(crate) head: String,
    /// Every PR (issue) comment.
    #[serde(default)]
    pub(crate) comments: Vec<Comment>,
    /// Every review, COMMENT-type critic verdicts included.
    #[serde(default)]
    pub(crate) reviews: Vec<Review>,
}

impl VerdictView {
    /// The head sha the verdicts were assessed against, lowercased, if valid.
    ///
    /// #10724: `tm pr merge` passes it as `--match-head-commit`, so a push
    /// after the verdict read makes GitHub refuse the merge.
    /// Test: `pr_10724_the_merge_call_matches_the_assessed_head`.
    pub(crate) fn head_oid(&self) -> Option<String> {
        pin_oid(&self.head)
    }
}

/// A comment or review author.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Author {
    #[serde(default)]
    pub(crate) login: String,
}

/// A PR comment.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Comment {
    #[serde(default)]
    pub(crate) author: Option<Author>,
    /// `OWNER`, `MEMBER`, `COLLABORATOR`, `CONTRIBUTOR`, `NONE`, ...
    #[serde(default, rename = "authorAssociation")]
    pub(crate) author_association: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default, rename = "createdAt")]
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) url: String,
}

/// The commit a review was submitted against.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CommitRef {
    #[serde(default)]
    pub(crate) oid: String,
}

/// A PR review.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Review {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) author: Option<Author>,
    /// The author's role on the repo, as on [`Comment`].
    #[serde(default, rename = "authorAssociation")]
    pub(crate) author_association: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default, rename = "submittedAt")]
    pub(crate) submitted_at: String,
    #[serde(default)]
    pub(crate) commit: Option<CommitRef>,
}

/// A verdict found on the PR, with the commit it covers.
#[derive(Debug, Clone)]
struct Posted {
    verdict: Verdict,
    /// Lowercase hex, 7 to 40 chars; `None` when nothing pins it.
    sha: Option<String>,
    /// RFC 3339 UTC timestamp; orders the verdicts.
    at: String,
    /// Where it was posted, for the refusal text.
    source: String,
}

/// What [`assess`] concluded.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Assessment {
    /// Refuse the merge with this reason.
    pub(crate) refusal: Option<String>,
    /// Merge, but print this.
    pub(crate) warning: Option<String>,
}

/// Fetch the head sha, comments and reviews in one read.
///
/// What: runs `base` (a `gh pr view <n> [--repo]` argv) plus
/// `--json headRefOid,comments,reviews`; a failed run or unparseable JSON is
/// an error, which the caller turns into a refusal.
/// Test: `pr_10724_a_verdict_read_failure_refuses`,
/// `pr_10724_unparseable_verdict_json_is_an_error`.
pub(crate) fn read<R: GhRunner>(gh: &R, mut base: Vec<String>) -> anyhow::Result<VerdictView> {
    base.push("--json".to_string());
    base.push("headRefOid,comments,reviews".to_string());
    let stdout = gh.run(&base)?.stdout_ok(&base)?;
    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("cannot parse `gh {}` JSON: {e}", base.join(" ")))
}

/// The first verdict line in `body`, with its `pinned to` sha.
///
/// Why: the critic decorates the Output Format heading (`# `, `**bold**`), and
/// a decorated BLOCK that went unread let the merge through; matching at line
/// start only keeps prose such as "said Verdict: BLOCK" from holding a merge.
/// What: the first unfenced line [`verdict_on_line`] reads wins. #10724: a
/// fence that never closes fails closed — a BLOCK after the unclosed opener
/// beats an earlier APPROVE or WARN, while an APPROVE or WARN there is ignored.
/// Test: `pr_10724_prose_and_fenced_lines_are_not_verdicts`,
/// `pr_10724_decorated_verdict_lines_parse`,
/// `pr_10724_an_unclosed_fence_does_not_hide_a_later_block`,
/// `pr_10724_a_pin_clause_is_parsed_from_the_verdict_line`.
pub(crate) fn parse_verdict_line(body: &str) -> Option<(Verdict, Option<String>)> {
    let mut fenced = false;
    let mut first: Option<(Verdict, Option<String>)> = None;
    // The first BLOCK seen inside a fence; it counts only if that fence never closes.
    let mut open_block: Option<(Verdict, Option<String>)> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            open_block = None;
            continue;
        }
        let Some(found) = verdict_on_line(line) else {
            continue;
        };
        if !fenced {
            if first.is_none() {
                first = Some(found);
            }
        } else if found.0 == Verdict::Block && open_block.is_none() {
            open_block = Some(found);
        }
    }
    let first_is_block = first.as_ref().is_some_and(|f| f.0 == Verdict::Block);
    if fenced && open_block.is_some() && !first_is_block {
        return open_block;
    }
    first
}

/// The verdict on one trimmed line, decorated or not.
///
/// What: the strict form first, then [`loose_block`].
/// Test: `pr_10724_decorated_verdict_lines_parse`,
/// `pr_10724_decorated_block_forms_fail_closed`.
fn verdict_on_line(line: &str) -> Option<(Verdict, Option<String>)> {
    strict_verdict(line).or_else(|| loose_block(line))
}

/// The strict verdict form, the only one an APPROVE or WARN may take.
///
/// What: strips leading `#` marks and `**` around the label and the value,
/// then requires `Verdict:` followed by exactly `APPROVE`, `WARN` or `BLOCK`
/// ending at a non-letter. The sha is the 7-40 hex token after `pinned to `.
/// Test: `pr_10724_decorated_verdict_lines_parse`.
fn strict_verdict(line: &str) -> Option<(Verdict, Option<String>)> {
    let line = line.trim_start_matches('#').trim_start();
    let line = line.strip_prefix(BOLD).unwrap_or(line);
    let rest = line.strip_prefix(VERDICT_LABEL)?;
    let rest = rest.strip_prefix(BOLD).unwrap_or(rest).trim_start();
    let rest = rest.strip_prefix(BOLD).unwrap_or(rest);
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let verdict = match &rest[..end] {
        "APPROVE" => Verdict::Approve,
        "WARN" => Verdict::Warn,
        "BLOCK" => Verdict::Block,
        _ => return None,
    };
    Some((verdict, pin(&rest[end..])))
}

/// A BLOCK in any case and decoration (#10796 review, H2).
///
/// Why: a BLOCK the parser cannot read lets the merge through, so BLOCK
/// matching is lenient while APPROVE and WARN stay strict.
/// What: drops leading marks that are not letters or digits (a `>` quote
/// excepted, so a quoted verdict stays inert), then wants a case-insensitive
/// `verdict` label, a separator of non-alphanumerics holding a `:`, and the
/// word `block` ending at a non-alphanumeric.
/// Test: `pr_10724_decorated_block_forms_fail_closed`,
/// `pr_10724_prose_and_fenced_lines_are_not_verdicts`.
fn loose_block(line: &str) -> Option<(Verdict, Option<String>)> {
    let line = line.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '>');
    let label = "verdict";
    if !line.get(..label.len())?.eq_ignore_ascii_case(label) {
        return None;
    }
    let rest = &line[label.len()..];
    let word_at = rest.find(char::is_alphanumeric)?;
    if !rest[..word_at].contains(':') {
        return None;
    }
    let rest = &rest[word_at..];
    let word = "block";
    if !rest.get(..word.len())?.eq_ignore_ascii_case(word) {
        return None;
    }
    let tail = &rest[word.len()..];
    if tail.chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }
    Some((Verdict::Block, pin(tail)))
}

/// Whether a verdict by an author with `association` counts (#10796 review, H1).
///
/// Why: the repo is public, so any GitHub user can post a verdict line.
/// What: a [`TRUSTED`] role always counts; an APPROVE or WARN from anyone else
/// never does; a BLOCK counts when the payload names no role at all, so a
/// payload missing the field fails closed.
/// Test: `pr_10724_an_untrusted_approve_does_not_clear_a_head_block`,
/// `pr_10724_an_untrusted_block_is_ignored_and_an_unknown_one_holds`.
fn admits(verdict: Verdict, association: &str) -> bool {
    let role = association.trim();
    TRUSTED.iter().any(|t| role.eq_ignore_ascii_case(t))
        || (verdict == Verdict::Block && role.is_empty())
}

/// The sha after `pinned to ` in `tail`, lowercased, when it is 7-40 hex.
fn pin(tail: &str) -> Option<String> {
    let after = &tail[tail.find(PIN_CLAUSE)? + PIN_CLAUSE.len()..];
    let token: String = after
        .trim_start_matches(['`', ' '])
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (7..=40)
        .contains(&token.len())
        .then(|| token.to_ascii_lowercase())
}

/// Every verdict on the PR that [`admits`] lets count, oldest first.
fn collect(view: &VerdictView) -> Vec<Posted> {
    let mut out: Vec<Posted> = Vec::new();
    for c in &view.comments {
        if let Some((verdict, sha)) = parse_verdict_line(&c.body)
            && admits(verdict, &c.author_association)
        {
            out.push(Posted {
                verdict,
                sha,
                at: c.created_at.clone(),
                source: format!("comment {}", c.url),
            });
        }
    }
    for r in &view.reviews {
        if let Some((verdict, text_pin)) = parse_verdict_line(&r.body)
            && admits(verdict, &r.author_association)
        {
            let oid = r.commit.as_ref().and_then(|c| pin_oid(&c.oid));
            let by = r.author.as_ref().map_or("", |a| a.login.as_str());
            out.push(Posted {
                verdict,
                sha: text_pin.or(oid),
                at: r.submitted_at.clone(),
                source: format!("review {} by {by}", r.id),
            });
        }
    }
    out.sort_by(|a, b| a.at.cmp(&b.at));
    out
}

/// A review's `commit.oid`, lowercased, when it is a full or short sha.
fn pin_oid(oid: &str) -> Option<String> {
    let oid = oid.trim();
    ((7..=40).contains(&oid.len()) && oid.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| oid.to_ascii_lowercase())
}

/// Judge the PR's code-critic verdicts against its head.
///
/// Why: a push does not resolve a BLOCK — the fix-round push, an unrelated
/// commit or `gh pr update-branch` all move the head (#10796 review, H3a). A
/// BLOCK that names no commit cannot be matched to one, so it fails closed.
/// What: refuses when the latest verdict pinned to the head is a BLOCK, when
/// an unpinned BLOCK has no later APPROVE/WARN pinned to the head, when a
/// BLOCK on an earlier commit has no later APPROVE/WARN on any commit, or
/// when verdicts exist and `headRefOid` is empty. Warns when verdicts exist
/// and none is pinned to the head. No verdict at all: neither.
///
/// Test: `pr_10724_the_head_block_refusal_names_source_and_sha`,
/// `pr_10724_an_old_head_approve_warns_that_no_verdict_covers_the_head`,
/// `pr_10724_an_old_head_block_refuses_until_a_later_verdict`,
/// `pr_10724_a_later_warn_resolves_a_head_block`,
/// `pr_10724_a_head_block_after_an_approve_refuses`,
/// `pr_10724_an_unpinned_block_names_the_fix`,
/// `pr_10724_an_unpinned_block_is_resolved_by_a_later_head_approve`,
/// `pr_10724_no_verdict_is_neither_refusal_nor_warning`,
/// `pr_10724_verdicts_without_a_head_sha_refuse`.
pub(crate) fn assess(view: &VerdictView) -> Assessment {
    let posted = collect(view);
    if posted.is_empty() {
        return Assessment::default();
    }
    let head = view.head.trim().to_ascii_lowercase();
    if pin_oid(&head).is_none() {
        return refuse(format!(
            "`gh pr view` reported no head sha, so the {} code-critic verdict(s) on the PR \
             cannot be matched to a commit",
            posted.len()
        ));
    }
    let covers = |sha: &str| head.starts_with(sha);
    let mut last_on_head: Option<&Posted> = None;
    let mut open_unpinned: Option<&Posted> = None;
    // #10796 review (H3a): a BLOCK on an earlier commit, not yet answered.
    let mut open_earlier: Option<&Posted> = None;
    for p in &posted {
        match (&p.sha, p.verdict) {
            (Some(sha), v) if covers(sha) => {
                last_on_head = Some(p);
                if v != Verdict::Block {
                    open_unpinned = None;
                    open_earlier = None;
                }
            }
            (Some(_), Verdict::Block) => open_earlier = Some(p),
            (Some(_), _) => open_earlier = None,
            (None, Verdict::Block) => open_unpinned = Some(p),
            (None, _) => {}
        }
    }
    if let Some(p) = last_on_head.filter(|p| p.verdict == Verdict::Block) {
        return refuse(format!(
            "an unresolved code-critic BLOCK is pinned to the head {head} ({}) — fix the \
             findings and get a later APPROVE or WARN verdict pinned to {head}",
            p.source
        ));
    }
    if let Some(p) = open_unpinned {
        return refuse(format!(
            "a code-critic BLOCK names no commit sha ({}), so it holds the merge — post a later \
             APPROVE or WARN pinned to the head {head}: a COMMENT review on that commit, or a \
             comment whose verdict line reads `## Verdict: APPROVE, pinned to {head}`, by an \
             OWNER, MEMBER or COLLABORATOR",
            p.source
        ));
    }
    if let Some(p) = open_earlier {
        return refuse(format!(
            "a code-critic BLOCK on the earlier commit {} ({}) has no later APPROVE or WARN — a \
             push does not resolve it; get a code-critic verdict on the head {head}",
            p.sha.as_deref().unwrap_or_default(),
            p.source
        ));
    }
    if last_on_head.is_some() {
        return Assessment::default();
    }
    let latest = posted.last().map_or(String::new(), |p| {
        format!(
            "; the latest is {} pinned to {} ({})",
            p.verdict.label(),
            p.sha.as_deref().unwrap_or("no commit"),
            p.source
        )
    });
    Assessment {
        refusal: None,
        warning: Some(format!(
            "no code-critic verdict covers the head {head}{latest}"
        )),
    }
}

fn refuse(reason: String) -> Assessment {
    Assessment {
        refusal: Some(reason),
        warning: None,
    }
}

#[cfg(test)]
#[path = "critic_verdict_tests.rs"]
mod tests;
