//! Which embedder produced an index's vectors, and whether it is the running one (#10664).
//!
//! Why: an index can hold vectors from one embedder (INT8 MiniLM) while the
//! daemon now embeds queries and new chunks with another (fp32 MiniLM). Both
//! are 384-dimensional, so nothing failed and nothing detected it.
//! What: [`EmbedderIdentity`] is the stamp a build records in the corpus
//! `_meta` table; [`IdentityCheck::compare`] compares it EXACTLY with the
//! running embedder's identity. The check is report-only: it never refuses a
//! load, re-embeds or deletes. No tolerance is applied; that decision stays
//! open on #10664.
//! Test: `core::indexer::tests::embedder_identity_10664`.

use serde::{Deserialize, Serialize};

/// The identity of the embedder that produced a set of vectors (#10664).
///
/// Why: the issue's criterion (1) asks for model and quantization; the
/// dimension is added because it is what the running embedder reports.
/// What: `model` (`all-MiniLM-L6-v2`), `dimension` (384), `precision`
/// (`fp32` or `int8`). Unknown fields are ignored on read, so a later
/// binary may add one without an older binary failing to read the stamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedderIdentity {
    pub model: String,
    pub dimension: usize,
    pub precision: String,
}

impl EmbedderIdentity {
    /// Build an identity; `quantized` maps to `int8`, otherwise `fp32`.
    pub fn new(model: impl Into<String>, dimension: usize, quantized: bool) -> Self {
        Self {
            model: model.into(),
            dimension,
            precision: if quantized { "int8" } else { "fp32" }.to_string(),
        }
    }
}

impl std::fmt::Display for EmbedderIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}d", self.model, self.precision, self.dimension)
    }
}

/// What a corpus holds under the identity key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StampRead {
    /// No stamp: the index was built before #10664, or has no corpus.
    Absent,
    /// A stamp that decoded.
    Stamped(EmbedderIdentity),
    /// The stamp could not be read or decoded; the string says why.
    Unreadable(String),
}

/// The result of comparing a stored stamp with the running embedder (#10664).
///
/// Why: a read or decode failure must never read as a match (Fail-Open
/// Check), and an index with no stamp must not read as a mismatch.
/// What: one variant per outcome; [`Self::status`] is the label the status
/// output carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityCheck {
    /// Stored and running identity are equal in every field.
    Match(EmbedderIdentity),
    /// Stored and running identity differ in at least one field.
    Mismatch {
        stored: EmbedderIdentity,
        active: EmbedderIdentity,
    },
    /// No stamp is stored.
    Unknown,
    /// The stamp exists but could not be read or decoded.
    Unreadable(String),
    /// A stamp is stored but the running embedder exposes no identity.
    ActiveUnknown(EmbedderIdentity),
}

impl IdentityCheck {
    /// Compare `stored` with `active` by exact equality of all fields.
    ///
    /// Why: criterion (3), the tolerance, is undecided, so no near-match is
    /// treated as a match.
    /// What: absent → `Unknown`; unreadable → `Unreadable`; no active
    /// identity → `ActiveUnknown`; equal → `Match`; otherwise `Mismatch`.
    /// Test: `compare_covers_every_outcome`.
    pub fn compare(stored: StampRead, active: Option<EmbedderIdentity>) -> Self {
        match (stored, active) {
            (StampRead::Absent, _) => Self::Unknown,
            (StampRead::Unreadable(reason), _) => Self::Unreadable(reason),
            (StampRead::Stamped(stored), None) => Self::ActiveUnknown(stored),
            (StampRead::Stamped(stored), Some(active)) if stored == active => Self::Match(stored),
            (StampRead::Stamped(stored), Some(active)) => Self::Mismatch { stored, active },
        }
    }

    /// The status label: `match`, `mismatch`, `unknown`, `unreadable` or `active_unknown`.
    pub fn status(&self) -> &'static str {
        match self {
            Self::Match(_) => "match",
            Self::Mismatch { .. } => "mismatch",
            Self::Unknown => "unknown",
            Self::Unreadable(_) => "unreadable",
            Self::ActiveUnknown(_) => "active_unknown",
        }
    }

    /// The `embedder_identity` object of `GET /indexes/:id/status`.
    ///
    /// What: `{status, stored, active, reason}`; a field that does not apply
    /// is `null`. `active` is the running embedder's identity whatever the
    /// status, so an `unknown` index still shows what would be stamped.
    /// Test: `the_status_body_carries_the_identity_check`.
    pub fn to_json(&self, active: Option<&EmbedderIdentity>) -> serde_json::Value {
        let (stored, reason) = match self {
            Self::Match(s) | Self::ActiveUnknown(s) | Self::Mismatch { stored: s, .. } => {
                (Some(s), None)
            }
            Self::Unreadable(r) => (None, Some(r.as_str())),
            Self::Unknown => (None, None),
        };
        serde_json::json!({
            "status": self.status(),
            "stored": stored,
            "active": active,
            "reason": reason,
        })
    }

    /// Log the outcome of a load: one WARN for a mismatch or an unreadable
    /// stamp, nothing otherwise.
    ///
    /// Why: criterion (2) — a load must report a store whose recorded
    /// identity differs from the running embedder.
    /// What: names both identities on a mismatch. Never changes the index.
    /// Test: `a_mismatched_stamp_is_reported_and_the_index_still_loads`,
    /// `a_matching_stamp_reports_nothing`.
    pub fn log_at_load(&self, index_id: &str) {
        match self {
            // #10664: report-only; the index loads and serves unchanged.
            Self::Mismatch { stored, active } => tracing::warn!(
                index_id,
                "embedder identity mismatch for '{index_id}': vectors were stamped by \
                 [{stored}], the running embedder is [{active}]. The index loads unchanged; \
                 see #10664"
            ),
            Self::Unreadable(reason) => tracing::warn!(
                index_id,
                "embedder identity stamp for '{index_id}' is unreadable ({reason}); not \
                 treated as a match. The index loads unchanged; see #10664"
            ),
            Self::Match(_) | Self::Unknown | Self::ActiveUnknown(_) => {}
        }
    }
}

/// Whether a completed reindex may stamp the running embedder's identity.
///
/// Why: a stamp claims every vector came from one embedder. An incremental
/// run re-embeds only changed files, so stamping it would hide a mixed store.
/// What: `true` for a forced run, or for a run whose hash cache started
/// empty, where every walked file was re-parsed and its vectors replaced.
/// Test: `should_stamp_only_full_rebuilds`.
pub fn should_stamp(force: bool, hashes_loaded: usize) -> bool {
    force || hashes_loaded == 0
}
