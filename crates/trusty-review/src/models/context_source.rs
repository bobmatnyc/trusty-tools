//! Which optional context sources a review used (#9192, for #9194).
//!
//! Why: a caller that asks for an optional input needs to know whether it
//! reached the reviewer, whole or cut, or could not be read at all.
//! What: one [`ContextSourceRecord`] per source, with per-item rows where a
//! source has several parts. `run_review_with` returns the records in
//! `ReviewOutcome::context_sources`; the MCP envelope carries them as
//! `context_sources`. `ReviewResult` is unchanged.
//! Test: `ledger_records_pr_body_used_truncated_absent_unavailable`,
//! `context_sources_serialize_in_snake_case`.

use serde::{Deserialize, Serialize};

/// What happened to one requested context source.
///
/// Why: "the body was empty" and "the body could not be fetched" call for
/// different caller action, so they are separate states.
/// What: `Used` reached the reviewer whole; `Truncated` reached it cut at its
/// cap with a visible marker; `Absent` had no text; `Unavailable` could not be
/// read, with the reason in `detail`; `Omitted` (items only, #9197) was left
/// out whole, with the reason in `detail`; `NotRequested` (#9194) was not
/// asked for, with the reason in `detail`.
/// Test: `context_sources_serialize_in_snake_case`,
/// `more_than_eight_docs_drop_the_tail_with_omitted_records`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceState {
    /// Reached the reviewer whole.
    Used,
    /// Reached the reviewer cut at its cap, with a visible marker.
    Truncated,
    /// Requested, but there was no text.
    Absent,
    /// Requested, but it could not be read; `detail` names why.
    Unavailable,
    /// An item left out whole — over a count or size cap, or a duplicate;
    /// `detail` names which (#9197).
    Omitted,
    /// The request did not ask for this source; `detail` names why (#9194).
    NotRequested,
}

/// One part of a source, such as one caller-context field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ContextItemRecord {
    /// The part's name, such as `pr_description`.
    pub id: String,
    /// What happened to it.
    pub state: SourceState,
    /// Characters that reached the reviewer, excluding any truncation marker.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chars: usize,
    /// Characters cut by the cap.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chars_omitted: usize,
    /// Why the item was omitted or unavailable (#9197).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One requested context source and what happened to it.
///
/// Why: the ledger a caller reads to see which sources a review used.
/// What: `source` names it (`pr_body`, `caller_context`); `chars` and
/// `chars_omitted` count what reached the reviewer and what the cap cut;
/// `detail` explains an `unavailable` state; `items` lists the parts.
/// Test: `ledger_records_pr_body_used_truncated_absent_unavailable`,
/// `a_new_input_turns_the_ledger_on`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ContextSourceRecord {
    /// The source's name.
    pub source: String,
    /// What happened to it.
    pub state: SourceState,
    /// Characters that reached the reviewer, excluding any truncation marker.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chars: usize,
    /// Characters cut by the cap.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chars_omitted: usize,
    /// Why the source was unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The source's parts, when it has several.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ContextItemRecord>,
}

impl ContextItemRecord {
    /// A row for `id` with `chars` characters kept and `chars_omitted` cut.
    pub fn new(id: &str, state: SourceState, chars: usize, chars_omitted: usize) -> Self {
        Self {
            id: id.to_string(),
            state,
            chars,
            chars_omitted,
            detail: None,
        }
    }
}

impl ContextSourceRecord {
    /// A record for `source` with no counts, detail or items.
    pub fn new(source: &str, state: SourceState) -> Self {
        Self {
            source: source.to_string(),
            state,
            chars: 0,
            chars_omitted: 0,
            detail: None,
            items: Vec::new(),
        }
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #9192: states and keys serialize in snake case; zero counts, a missing
    /// detail and empty items are left out.
    #[test]
    fn context_sources_serialize_in_snake_case() {
        let mut row = ContextSourceRecord::new("caller_context", SourceState::Truncated);
        row.chars = 64_000;
        row.chars_omitted = 6;
        row.items = vec![ContextItemRecord::new(
            "pr_discussion",
            SourceState::Absent,
            0,
            0,
        )];
        let json = serde_json::to_value(&row).expect("serialises");
        assert_eq!(
            json,
            serde_json::json!({
                "source": "caller_context", "state": "truncated",
                "chars": 64_000, "chars_omitted": 6,
                "items": [{"id": "pr_discussion", "state": "absent"}],
            })
        );
        let back: ContextSourceRecord = serde_json::from_value(json).expect("round-trips");
        assert_eq!(back, row);
        let down = ContextSourceRecord::new("pr_body", SourceState::Unavailable);
        assert_eq!(
            serde_json::to_value(&down).expect("serialises")["state"],
            "unavailable"
        );
    }
}
