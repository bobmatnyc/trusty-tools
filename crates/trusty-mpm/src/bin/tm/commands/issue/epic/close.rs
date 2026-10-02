//! `tm issue epic close` — close a tracker once every phase is closed (#8448).
//!
//! Why: `TICKETING.md` closes a tracker "when every phase is closed and each
//! outcome is verified, with a closing comment mapping O1..On to evidence, one
//! line each". Done by hand, the first half is a glance at a sub-issue list and
//! the second is prose — so a tracker could close over an open phase, or with a
//! comment that named three of four outcomes. Both checks are mechanical here.
//! What: [`close`] enumerates the tracker's native sub-issues and refuses while
//! any is open, naming each; reads the outcomes the tracker body declares and
//! refuses unless the caller supplied evidence for exactly that set; posts the
//! closing comment; then closes the issue.
//!
//! # The two-call tail
//!
//! The comment and the close are two `gh` calls. A process KILLED between them
//! leaves the comment posted and the tracker open; the re-run finds the comment
//! by its [`CLOSE_COMMENT_PREFIX`] and skips straight to the close, so the
//! record carries one closing comment, not two. A REPORTED failure on either
//! call reaches the caller with the tracker named.
//!
//! Test: `close_refuses_while_a_child_is_open_and_names_it`,
//! `close_succeeds_once_every_child_is_closed`,
//! `close_posts_one_line_per_declared_outcome`,
//! `close_refuses_a_tracker_with_no_children`,
//! `close_refuses_a_tracker_that_declares_no_outcomes`,
//! `close_refuses_evidence_that_does_not_match_the_declared_outcomes`,
//! `close_does_not_post_a_second_comment_on_re_run`,
//! `close_propagates_a_failed_close_call`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::backend::EpicBackend;
use super::render;

/// The first line of the closing comment, matched literally on a re-run.
pub(crate) const CLOSE_COMMENT_PREFIX: &str = "tm issue epic close:";

/// What one `close` run did.
///
/// Test: `close_succeeds_once_every_child_is_closed`.
#[derive(Debug, Clone)]
pub(crate) struct CloseReport {
    /// The tracker closed.
    pub(crate) tracker: u64,
    /// The closed children, in number order.
    pub(crate) phases: Vec<u64>,
    /// How many outcome lines the closing comment carries.
    pub(crate) outcomes: usize,
    /// Whether this run posted the comment (`false` when a re-run found it).
    pub(crate) comment_posted: bool,
}

/// Close `tracker` once every native sub-issue is closed.
///
/// Why: AC3 — an open child is a refusal that names it, and the comment carries
/// one line per declared outcome. The child set is the ENUMERATED sub-issue
/// connection ([`EpicBackend::children`] pages a short one), so "no open child"
/// is never a truncated page's verdict; and an EMPTY set is a refusal too,
/// because a tracker with no linked phases has nothing this verb can vouch for.
/// What: reads the children, refuses on none or on any open one; reads the
/// body's outcomes and refuses on none; matches `evidence` (`O<n>: <text>`, one
/// per outcome) against the declared set and refuses on a missing, unknown or
/// repeated id; posts the comment unless one is already on the record; closes.
/// Test: see the module doc.
pub(crate) fn close<B: EpicBackend>(
    backend: &B,
    tracker: u64,
    evidence: &[String],
) -> anyhow::Result<CloseReport> {
    let children = backend.children(tracker)?;
    if children.is_empty() {
        anyhow::bail!(
            "#{tracker} has no native sub-issues — nothing to close as an epic. Link its phases \
             with `gh issue edit {tracker} --add-sub-issue <number>` first, or close it by hand"
        );
    }
    let open: Vec<String> = children
        .iter()
        .filter(|c| !c.state.eq_ignore_ascii_case("CLOSED"))
        .map(|c| format!("#{} `{}`", c.number, c.title.trim()))
        .collect();
    if !open.is_empty() {
        anyhow::bail!(
            "#{tracker} still has {} open child issue(s): {} — close them first",
            open.len(),
            open.join(", ")
        );
    }

    let body = backend.body(tracker)?;
    let outcomes = render::outcomes_of(&body);
    if outcomes.is_empty() {
        anyhow::bail!(
            "#{tracker}'s body declares no `- **O<n>** …` outcomes under `## Outcomes`, so the \
             closing comment has nothing to map evidence to — add them, or close it by hand"
        );
    }
    let evidence = evidence_by_outcome(evidence, &outcomes)?;

    let mut phases: Vec<u64> = children.iter().map(|c| c.number).collect();
    phases.sort_unstable();
    let mut comment = format!(
        "{CLOSE_COMMENT_PREFIX} every phase is closed — {}.\n",
        phases
            .iter()
            .map(|n| format!("#{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (id, text) in &outcomes {
        let proof = evidence
            .get(id.as_str())
            .ok_or_else(|| anyhow::anyhow!("internal: evidence for {id} vanished"))?;
        let _ = write!(comment, "\n- **{id}** {text} — {proof}");
    }
    comment.push('\n');

    // #8448: a run killed between the comment and the close leaves the comment
    // on the record; the re-run must finish the close without a second one.
    let already = backend
        .comments(tracker)?
        .iter()
        .any(|c| c.trim_start().starts_with(CLOSE_COMMENT_PREFIX));
    if !already {
        backend.comment(tracker, &comment)?;
    }
    backend
        .close_issue(tracker)
        .map_err(|e| anyhow::anyhow!("#{tracker}: the closing comment is posted but the issue is still open ({e}) — re-run to finish the close"))?;
    Ok(CloseReport {
        tracker,
        phases,
        outcomes: outcomes.len(),
        comment_posted: !already,
    })
}

/// Match the `--evidence` entries to the declared outcomes, one each.
///
/// Why: "one line per outcome" is only true when the caller supplied one proof
/// per declared id — a missing id would leave an outcome unverified on the
/// record, an unknown one would assert an outcome the tracker never declared.
/// What: each entry is `O<n>: <text>` (a space after the id also parses); the
/// ids must be exactly the declared set, with no repeat.
/// Test: `close_refuses_evidence_that_does_not_match_the_declared_outcomes`.
fn evidence_by_outcome<'a>(
    evidence: &'a [String],
    outcomes: &[(String, String)],
) -> anyhow::Result<BTreeMap<&'a str, &'a str>> {
    let mut by_id: BTreeMap<&str, &str> = BTreeMap::new();
    for entry in evidence {
        let trimmed = entry.trim();
        let split = trimmed
            .find([':', ' '])
            .map(|i| (&trimmed[..i], trimmed[i + 1..].trim()));
        let Some((id, text)) = split.filter(|(id, text)| !id.is_empty() && !text.is_empty()) else {
            anyhow::bail!(
                "`--evidence {entry:?}` is not `O<n>: <what proves it>` — one entry per outcome"
            );
        };
        if !id.starts_with('O') || !id[1..].chars().all(|c| c.is_ascii_digit()) || id.len() < 2 {
            anyhow::bail!("`--evidence {entry:?}` names `{id}`, which is not an `O<n>` outcome id");
        }
        if by_id.insert(id, text).is_some() {
            anyhow::bail!("`--evidence` names {id} twice — one entry per outcome");
        }
    }
    let declared: Vec<&str> = outcomes.iter().map(|(id, _)| id.as_str()).collect();
    let missing: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|id| !by_id.contains_key(id))
        .collect();
    let unknown: Vec<&str> = by_id
        .keys()
        .copied()
        .filter(|id| !declared.contains(id))
        .collect();
    if !missing.is_empty() || !unknown.is_empty() {
        anyhow::bail!(
            "the tracker declares outcomes [{}]; `--evidence` is missing [{}] and names unknown \
             [{}] — pass exactly one `--evidence \"O<n>: <what proves it>\"` per declared outcome",
            declared.join(", "),
            missing.join(", "),
            unknown.join(", ")
        );
    }
    Ok(by_id)
}
