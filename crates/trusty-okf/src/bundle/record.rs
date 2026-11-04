//! The verification record and the verification axis (SPEC-OKF-01 §2.10,
//! §3 (d) C, §4.9 Axis 3).
//!
//! Why: under 3d C the record outside frontmatter is authoritative. Only an
//! event in the record, bound to the current content hash, counts as
//! Verified; an event that exists only in frontmatter, or whose hash no
//! longer matches, is history.
//! What: one append-only JSON-lines file per concept at
//! `_sources/verification/<id>.jsonl`. Each line is one event state:
//! `by`, `at`, the `hash` it was bound to (absent for a legacy event), and
//! once invalidated `invalidated_at`, `invalidated_by_hash` and an optional
//! `reason` (`legacy_unbound`). The newest line for one `(by, at)` pair is
//! that event's state. [`Bundle::verification_events`] and
//! [`Bundle::verification_state`] read it; `Bundle::revision_state`
//! (#10746 S5) composes this axis with the save and index axes.
//! Test: `c31_body_change_moves_verification_to_the_record`,
//! `c36_out_of_band_edit_reads_unverified_with_a_warning`.

use chrono::{DateTime, FixedOffset, SecondsFormat};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::paths::{confined_file, io_err};
use crate::format::concept::Concept;
use crate::format::fields::{Actor, Verification, parse_instant};
use crate::format::hash::{ContentHash, content_hash};
use crate::format::id::ConceptId;

/// One verification event with its binding (SPEC-OKF-01 §4.2).
///
/// #10746: `hash` is optional and `reason` is added, because a legacy event
/// written before r3 carries no hash and moves to history with reason
/// `legacy_unbound` (§2.10 "Events with no hash").
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationEvent {
    /// Who verified and when.
    pub v: Verification,
    /// The content hash the verifier saw; `None` for a legacy event.
    pub hash: Option<ContentHash>,
    /// When a content change moved the event to history.
    pub invalidated_at: Option<DateTime<FixedOffset>>,
    /// The content hash that replaced the verified revision.
    pub invalidated_by_hash: Option<ContentHash>,
    /// Why it moved, when not a plain content change.
    pub reason: Option<String>,
}

/// The verification axis of the current revision (SPEC-OKF-01 §4.2, §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationState {
    /// No record event is bound to the current content hash.
    Unverified {
        /// Events that no longer attest the current revision.
        history: usize,
    },
    /// At least one record event is bound to the current content hash.
    Verified {
        /// Whether a `human:` actor is among the current events.
        human: bool,
        /// Current events.
        events: usize,
        /// Events that no longer attest the current revision.
        history: usize,
    },
}

/// One line of the record file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Line {
    pub by: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidated_by_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// #10746 S5: the operation-journal op that appended it (§4.11 "Steps").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// #10746 S5: that operation's step index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<usize>,
}

impl Line {
    /// The line as a JSON value, for an operation append step.
    pub(crate) fn value(&self) -> Result<serde_json::Value, OkfError> {
        serde_json::to_value(self).map_err(|e| OkfError::Parse {
            path: "_sources/verification".into(),
            line: None,
            message: e.to_string(),
        })
    }
}

/// An instant as written into frontmatter and the record: RFC 3339, `Z` for
/// UTC, whole seconds unless the value has a fraction.
pub(crate) fn instant_string(t: &DateTime<FixedOffset>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

/// The record's bundle-relative path.
pub(crate) fn record_rel(id: &ConceptId) -> String {
    format!("_sources/verification/{}.jsonl", id.as_str())
}

/// Every line of `id`'s record, oldest first. A missing file is empty; a
/// torn or unparseable line is skipped (the record is append-only).
pub(crate) fn read_lines(b: &Bundle, id: &ConceptId) -> Result<Vec<Line>, OkfError> {
    let path = confined_file(b.root(), &record_rel(id))?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(&path, e)),
    };
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .collect())
}

/// The newest state of each `(by, at)` event, in first-seen order.
pub(crate) fn fold(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for line in lines {
        match out.iter_mut().find(|l| l.by == line.by && l.at == line.at) {
            Some(slot) => *slot = line,
            None => out.push(line),
        }
    }
    out
}

/// Whether an event state still attests `current`.
fn is_current(l: &Line, current: &ContentHash) -> bool {
    l.invalidated_at.is_none() && l.hash.as_deref() == Some(current.as_str())
}

/// The verification axis for folded event states and the current hash.
pub(crate) fn state_of(events: &[Line], current: &ContentHash) -> VerificationState {
    let live: Vec<&Line> = events.iter().filter(|l| is_current(l, current)).collect();
    let history = events.len() - live.len();
    if live.is_empty() {
        return VerificationState::Unverified { history };
    }
    VerificationState::Verified {
        human: live
            .iter()
            .any(|l| matches!(Actor::parse(&l.by), Actor::Human(_))),
        events: live.len(),
        history,
    }
}

/// Whether a bound, not-invalidated event names a hash other than `current`:
/// the content changed outside trusty-okf (§2.10 "Out-of-band edits").
pub(crate) fn has_mismatch(events: &[Line], current: &ContentHash) -> bool {
    events.iter().any(|l| {
        l.invalidated_at.is_none() && l.hash.as_deref().is_some_and(|h| h != current.as_str())
    })
}

/// The `(by, at)` strings of each well-formed event in a `verified` value
/// (a list, or a bare mapping read as a one-element list).
pub(crate) fn frontmatter_events(verified: Option<&Value>) -> Vec<(String, String)> {
    let items: Vec<&Value> = match verified {
        Some(Value::Sequence(seq)) => seq.iter().collect(),
        Some(one @ Value::Mapping(_)) => vec![one],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|v| {
            let by = v.get("by")?.as_str()?.to_string();
            let at = v.get("at")?.as_str()?.to_string();
            Some((by, at))
        })
        .collect()
}

/// The record lines that move frontmatter events to history on a content
/// change (§2.10 step 2). A bound event keeps its hash and gains the
/// invalidation; an event the record never bound is `legacy_unbound`; an
/// event already in history adds no line.
pub(crate) fn invalidation_lines(
    folded: &[Line],
    events: &[(String, String)],
    now: &str,
    new_hash: &ContentHash,
) -> Vec<Line> {
    let mut out = Vec::new();
    for (by, at) in events {
        let known = folded.iter().find(|l| &l.by == by && &l.at == at);
        if known.is_some_and(|l| l.invalidated_at.is_some()) {
            continue;
        }
        let hash = known.and_then(|l| l.hash.clone());
        let reason = hash.is_none().then(|| "legacy_unbound".to_string());
        out.push(Line {
            by: by.clone(),
            at: at.clone(),
            hash,
            invalidated_at: Some(now.to_string()),
            invalidated_by_hash: Some(new_hash.as_str().to_string()),
            reason,
            op: None,
            step: None,
        });
    }
    out
}

impl Bundle {
    /// Every event in `id`'s verification record, newest state per event.
    ///
    /// Why: the record is authoritative (3d C); a caller audits it here.
    /// What: folds the append-only record; a line whose `at` is not an
    /// instant is skipped.
    /// Test: `c31_body_change_moves_verification_to_the_record`.
    pub fn verification_events(&self, id: &ConceptId) -> Result<Vec<VerificationEvent>, OkfError> {
        let hash = |s: &Option<String>| s.as_deref().and_then(ContentHash::from_stored);
        Ok(fold(read_lines(self, id)?)
            .into_iter()
            .filter_map(|l| {
                Some(VerificationEvent {
                    v: Verification {
                        by: Actor::parse(&l.by),
                        at: parse_instant(&l.at)?,
                        stale_after: None,
                    },
                    hash: hash(&l.hash),
                    invalidated_at: l.invalidated_at.as_deref().and_then(parse_instant),
                    invalidated_by_hash: hash(&l.invalidated_by_hash),
                    reason: l.reason,
                })
            })
            .collect())
    }

    /// The verification axis of `id`'s current revision (§4.9 Axis 3).
    ///
    /// Why: the OKF trust tier reads raw frontmatter; trusty's own
    /// verification state reads only record events bound to the current
    /// content hash, so an out-of-band edit reads Unverified (criterion 36).
    /// What: reads the concept, hashes it and folds the record.
    /// Test: `c36_out_of_band_edit_reads_unverified_with_a_warning`,
    /// `c31_body_change_moves_verification_to_the_record`.
    pub fn verification_state(&self, id: &ConceptId) -> Result<VerificationState, OkfError> {
        let c = self.get(id)?;
        state_for(self, id, &c)
    }
}

/// The verification axis of an already-read concept.
pub(crate) fn state_for(
    b: &Bundle,
    id: &ConceptId,
    c: &Concept,
) -> Result<VerificationState, OkfError> {
    Ok(state_of(&fold(read_lines(b, id)?), &content_hash(c)))
}
