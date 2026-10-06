//! Caller context, the optional PR body, and the refs corpus (#9192).
//!
//! Why: the reviewer prompt, the verifier's author rationale, the refs corpus
//! and the map-reduce branch all read `CallerContext::pr_description`, so the
//! PR body is merged into that one field at one write site.
//! What: [`apply_caller_context`] records the caller fields, caps them
//! (#8654), and merges the capped PR body when the request asks for it;
//! [`refs_for_gate`] builds the corpus both review paths hand the citation
//! gate.
//! Test: `off_refs_corpus_is_byte_identical`,
//! `refs_use_the_capped_body_when_on`, `body_and_caller_field_are_capped_separately`.

use crate::{
    config::constants::{MAX_CALLER_CONTEXT_CHARS, MAX_PR_BODY_CHARS},
    models::{ContextItemRecord, ContextSourceRecord, SourceState},
    pipeline::{
        caller_preamble::cap_caller_context, runner::CallerContext, withheld_contract::refs_corpus,
    },
};

use super::{OptionalContextRequest, ledger::ContextLedger};

/// The heading caller text gets when it follows a merged PR body.
pub(crate) const CALLER_TEXT_HEADING: &str = "### Additional description from caller";

/// What the runner knows about the PR body.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PrBody<'a> {
    /// A local diff: there is no PR.
    Local,
    /// The metadata fetch failed with this error.
    Failed(&'a str),
    /// The fetched body, possibly empty.
    Fetched(&'a str),
}

impl<'a> PrBody<'a> {
    /// Classify the runner's step-2 values.
    pub(crate) fn of(is_local: bool, meta_error: Option<&'a str>, body: &'a str) -> Self {
        match (is_local, meta_error) {
            (true, _) => Self::Local,
            (false, Some(e)) => Self::Failed(e),
            (false, None) => Self::Fetched(body),
        }
    }
}

/// Record, cap, and optionally merge the PR body into `caller` (#9192).
///
/// Why: one call site, so the cap and the merge order cannot drift between
/// the reviewer prompt, the verifier and the refs corpus.
/// What: records the caller fields as they arrived (when the ledger is on),
/// caps each at `MAX_CALLER_CONTEXT_CHARS` (#8654), then, when
/// `include_pr_body` is on, writes the PR body capped at `MAX_PR_BODY_CHARS`
/// into `pr_description`, with any caller text after it under
/// [`CALLER_TEXT_HEADING`]. With the request off this is `cap_caller_context`.
/// Test: `caller_text_follows_the_fetched_body_not_over_it`,
/// `body_and_caller_field_are_capped_separately`,
/// `ledger_records_pr_body_used_truncated_absent_unavailable`.
pub(crate) fn apply_caller_context(
    caller: &mut CallerContext,
    request: &OptionalContextRequest,
    body: PrBody<'_>,
    ledger: &mut ContextLedger,
) {
    let caller_row = ledger.is_enabled().then(|| caller_record(caller));
    cap_caller_context(caller, MAX_CALLER_CONTEXT_CHARS);
    if request.include_pr_body {
        ledger.push(merge_pr_body(caller, body));
    }
    if let Some(row) = caller_row {
        ledger.push(row);
    }
}

/// Merge the capped PR body ahead of the caller's description; the ledger row.
fn merge_pr_body(caller: &mut CallerContext, body: PrBody<'_>) -> ContextSourceRecord {
    let text = match body {
        PrBody::Local => {
            return unavailable("local diff has no PR");
        }
        PrBody::Failed(e) => return unavailable(&format!("PR metadata fetch failed: {e}")),
        PrBody::Fetched(text) if text.trim().is_empty() => {
            return ContextSourceRecord::new("pr_body", SourceState::Absent);
        }
        PrBody::Fetched(text) => text,
    };
    let (kept, omitted) = counts(text, MAX_PR_BODY_CHARS);
    let mut merged = text.chars().take(kept).collect::<String>();
    if omitted > 0 {
        merged.push_str(&format!(
            "\n[... truncated: {omitted} more characters omitted; the PR body is capped at \
             {MAX_PR_BODY_CHARS} characters (#9192) ...]"
        ));
    }
    if let Some(own) = caller.pr_description.take()
        && !own.trim().is_empty()
    {
        merged.push_str(&format!("\n\n{CALLER_TEXT_HEADING}\n\n{own}"));
    }
    caller.pr_description = Some(merged);
    let mut row = ContextSourceRecord::new("pr_body", state_of(omitted));
    row.chars = kept;
    row.chars_omitted = omitted;
    row
}

/// The `caller_context` row, read before the cap so a cut has its size.
fn caller_record(caller: &CallerContext) -> ContextSourceRecord {
    let fields = [
        ("pr_description", &caller.pr_description),
        ("pr_discussion", &caller.pr_discussion),
        ("referenced_code", &caller.referenced_code),
    ];
    let items: Vec<ContextItemRecord> = fields
        .into_iter()
        .map(
            |(id, text)| match text.as_deref().filter(|t| !t.trim().is_empty()) {
                None => ContextItemRecord::new(id, SourceState::Absent, 0, 0),
                Some(t) => {
                    let (kept, omitted) = counts(t, MAX_CALLER_CONTEXT_CHARS);
                    ContextItemRecord::new(id, state_of(omitted), kept, omitted)
                }
            },
        )
        .collect();
    let state = if items.iter().any(|i| i.state == SourceState::Truncated) {
        SourceState::Truncated
    } else if items.iter().any(|i| i.state == SourceState::Used) {
        SourceState::Used
    } else {
        SourceState::Absent
    };
    let mut row = ContextSourceRecord::new("caller_context", state);
    row.chars = items.iter().map(|i| i.chars).sum();
    row.chars_omitted = items.iter().map(|i| i.chars_omitted).sum();
    row.items = items;
    row
}

/// `(kept, omitted)` characters of `text` under a `max` cap.
fn counts(text: &str, max: usize) -> (usize, usize) {
    let total = text.chars().count();
    (total.min(max), total.saturating_sub(max))
}

fn state_of(omitted: usize) -> SourceState {
    if omitted > 0 {
        SourceState::Truncated
    } else {
        SourceState::Used
    }
}

fn unavailable(detail: &str) -> ContextSourceRecord {
    let mut row = ContextSourceRecord::new("pr_body", SourceState::Unavailable);
    row.detail = Some(detail.to_string());
    row
}

/// The text a `[gh:]`/`[jira:]`/`[confluence:]` citation must resolve in.
///
/// Why: both review paths must hand the citation gate the same corpus.
/// What: joins title, `body`, external context and `caller` (`[pr_description,
/// pr_discussion, referenced_code]` as the reviewer saw them). `body` is the
/// raw PR body, or `None` when `include_pr_body` already put the capped body
/// into `pr_description`, so a cut-off tail is not citable.
/// Test: `off_refs_corpus_is_byte_identical`, `refs_use_the_capped_body_when_on`.
pub(crate) fn refs_for_gate(
    title: &str,
    body: Option<&str>,
    external: &str,
    caller: [Option<&str>; 3],
) -> String {
    let [description, discussion, referenced] = caller;
    refs_corpus(&[
        Some(title),
        body,
        Some(external),
        description,
        discussion,
        referenced,
    ])
}
