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

    /// This row with `detail`; an empty one is none (#9194).
    #[must_use]
    pub(crate) fn with_detail(mut self, detail: &str) -> Self {
        self.detail = (!detail.is_empty()).then(|| detail.to_string());
        self
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

    /// This record with `detail`; an empty one is none (#9194).
    #[must_use]
    pub(crate) fn with_detail(mut self, detail: &str) -> Self {
        self.detail = (!detail.is_empty()).then(|| detail.to_string());
        self
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

    /// #9194: the new state is `not_requested` on the wire.
    #[test]
    fn not_requested_serialises_snake_case() {
        let row = ContextSourceRecord::new("pr_body", SourceState::NotRequested)
            .with_detail("include_pr_body is off");
        assert_eq!(
            serde_json::to_value(&row).expect("serialises"),
            serde_json::json!({
                "source": "pr_body", "state": "not_requested",
                "detail": "include_pr_body is off",
            })
        );
    }

    /// #9194 AC4: a record of every state round-trips.
    #[test]
    fn ledger_record_round_trips_with_every_state() {
        for state in [
            SourceState::Used,
            SourceState::Truncated,
            SourceState::Absent,
            SourceState::Unavailable,
            SourceState::Omitted,
            SourceState::NotRequested,
        ] {
            let mut row = ContextSourceRecord::new("analyze", state).with_detail("why");
            row.items = vec![ContextItemRecord::new("hotspots", state, 3, 1).with_detail("x")];
            let json = serde_json::to_string(&row).expect("serialises");
            let back: ContextSourceRecord = serde_json::from_str(&json).expect("parses");
            assert_eq!(back, row, "{json}");
        }
    }

    /// #9194 AC4: a consumer that reads only `source` and `state` parses the
    /// new ledger, ignoring every key it does not know.
    #[test]
    fn new_ledger_json_parses_as_value_ignoring_unknown_keys() {
        #[derive(Deserialize)]
        struct Row {
            source: String,
            state: String,
        }
        let ledger = serde_json::json!([
            {"source": "search", "state": "unavailable", "detail": "trusty-search query failed: 500"},
            {"source": "analyze", "state": "used",
             "items": [{"id": "hotspots", "state": "used"}, {"id": "smells", "state": "absent"}]},
            {"source": "external_sources", "state": "not_requested", "chars": 0, "future": 1},
        ]);
        let rows: Vec<Row> = serde_json::from_value(ledger).expect("a consumer parses it");
        let pairs: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.source.as_str(), r.state.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("search", "unavailable"),
                ("analyze", "used"),
                ("external_sources", "not_requested")
            ]
        );
    }

    /// #9194 AC4: the `result` of a `run --json` payload with the ledger on
    /// parses as the unchanged `ReviewResult`.
    #[test]
    fn old_review_result_json_deserialises_from_the_wrapped_result_field() {
        let result = crate::models::ReviewResult::new("acme", "billing", 7, "Add Y", "u");
        let rows = [ContextSourceRecord::new("search", SourceState::Absent)];
        let wrapped = serde_json::json!({
            "result": crate::run_output::run_json_payload(&result),
            "context_sources": crate::run_output::ledger_value(&rows[..]),
        });
        let back: crate::models::ReviewResult =
            serde_json::from_value(wrapped["result"].clone()).expect("the result parses");
        assert_eq!(
            serde_json::to_value(&back).expect("serialises"),
            serde_json::to_value(&result).expect("serialises")
        );
    }

    /// #9194 AC4: the plain `ReviewResult` golden from #9192 still parses.
    #[test]
    fn plain_review_result_json_still_deserialises() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/optional_context_off/unified-result.json"
        );
        let text = std::fs::read_to_string(path).expect("golden is readable");
        let result: crate::models::ReviewResult =
            serde_json::from_str(&text).expect("the golden parses as a ReviewResult");
        assert_eq!(result.owner, "acme");
    }
}
