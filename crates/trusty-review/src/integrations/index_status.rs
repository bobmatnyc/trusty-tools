//! Per-index status types for `GET /indexes/{id}/status` (#6686).
//!
//! Why: `/health` counts registry handles and discards index ids, so a single
//! failed index anywhere on the host made every review on that host degrade —
//! including reviews whose own index was perfectly healthy (#6686). The
//! question the gate actually needs answered is "can the index THIS review
//! queries return results", and only the per-index endpoint answers it.
//! `/health` keeps exactly one job: is the daemon reachable and serving.
//!
//! What: mirrors the subset of the `GET /indexes/{id}/status` payload the gate
//! decides on — `stages` (lexical / semantic / graph, each with a status and an
//! optional failure reason), the derived `search_capabilities` array, and the
//! `corpus_open_failure` classification. Every field is `#[serde(default)]` and
//! unknown fields are discarded, so a trusty-search-side addition never breaks
//! parsing here. [`IndexStatusResponse::serving_state`] folds them into the same
//! [`ServingState`] the health path already produces, so `context_gate` matches
//! on one type regardless of which probe produced the verdict.
//!
//! Test: `index_status_*` tests at the foot of this file; no live daemon
//! required.

use serde::{Deserialize, Serialize};

use super::health::ServingState;

/// The wire value trusty-search sends for a stage that failed.
///
/// Why: `StageStatus` serialises snake_case (`crates/trusty-search/src/core/
/// registry.rs`), and this consumer deserialises the field as a plain string so
/// a future variant parses instead of erroring. One constant keeps the magic
/// string in a single place.
const STAGE_FAILED: &str = "failed";

/// One stage of the trusty-search staged pipeline, as reported per index.
///
/// Why: the failed LANES are what a reviewer needs named. "3 indexes degraded"
/// is not actionable; "index `workspace`: lexical, semantic and graph all
/// failed" is.
/// What: the stage's status string (`pending` / `in_progress` / `ready` /
/// `skipped` / `failed`) plus trusty-search's own failure reason when it set
/// one.
/// Test: `index_status_all_lanes_failed_reports_no_capabilities`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IndexStageReport {
    /// Stage lifecycle status, verbatim from trusty-search.
    #[serde(default)]
    pub status: String,
    /// trusty-search's own explanation when `status == "failed"`.
    #[serde(default)]
    pub failure: Option<String>,
}

impl IndexStageReport {
    /// Whether this stage failed and its lane therefore answers nothing.
    ///
    /// Why: `Failed` is the one status that means results are unavailable —
    /// `skipped` (a deliberate `--lexical-only` opt-out) and `pending` are not
    /// faults.
    /// What: case-insensitive match on the `failed` wire value.
    /// Test: `index_status_a_skipped_lane_is_not_a_failure`.
    pub fn has_failed(&self) -> bool {
        self.status.eq_ignore_ascii_case(STAGE_FAILED)
    }
}

/// The three staged-pipeline lanes of one index.
///
/// Why/What/Test: see [`IndexStageReport`]; this is the container the daemon
/// nests them in.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IndexStagesReport {
    /// BM25 / literal / exact-match lane.
    #[serde(default)]
    pub lexical: IndexStageReport,
    /// Vector lane.
    #[serde(default)]
    pub semantic: IndexStageReport,
    /// Knowledge-graph lane.
    #[serde(default)]
    pub graph: IndexStageReport,
}

impl IndexStagesReport {
    /// Name every lane whose status is `failed`, with its reason when present.
    ///
    /// Why: the banner reason must say WHICH lanes died. The counter arithmetic
    /// this replaces could not — it subtracted two host-wide totals and asserted
    /// "queries return LEXICAL results only" for the remainder, which was flatly
    /// wrong for an index whose lexical lane had also failed (#6686).
    /// What: a `lane: reason` (or bare `lane`) clause per failed stage, in
    /// pipeline order.
    /// Test: `index_status_all_lanes_failed_reports_no_capabilities`.
    pub fn failed_lanes(&self) -> Vec<String> {
        [
            ("lexical", &self.lexical),
            ("semantic", &self.semantic),
            ("graph", &self.graph),
        ]
        .into_iter()
        .filter(|(_, stage)| stage.has_failed())
        .map(|(name, stage)| match stage.failure.as_deref() {
            Some(reason) => format!("{name} ({reason})"),
            None => name.to_string(),
        })
        .collect()
    }
}

/// trusty-search's classification of a corpus that would not open.
///
/// Why: this is the one SILENT failure mode — trusty-search keeps serving such
/// an index and answers `200 OK` with an EMPTY result set, so a review against
/// it loses context with no error to notice.
/// What: the `kind` label, whether trusty-search considers it transient, and its
/// stage reason.
/// Test: `index_status_names_a_corpus_open_failure`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CorpusOpenFailure {
    /// Classified failure label (e.g. `open_timeout`, `format_mismatch`).
    #[serde(default)]
    pub kind: String,
    /// Whether a retry could plausibly succeed.
    #[serde(default)]
    pub transient: bool,
    /// trusty-search's own stage reason text.
    #[serde(default)]
    pub reason: Option<String>,
}

/// The subset of `GET /indexes/{id}/status` the required-context gate decides on
/// (#6686).
///
/// Why: this is the probe that replaced host-wide `/health` counters as the
/// source of the gate's verdict and of the degraded banner's reason. It is
/// per-index by construction, so an unrelated repo's broken index can no longer
/// degrade a review whose own index is healthy, and a genuinely broken target
/// index can no longer hide behind a clean host.
/// What: the fields that answer "can this index return results" — the three
/// stage reports, the `search_capabilities` array trusty-search derives from
/// them, and the corpus-open classification. Unknown fields are discarded.
/// Test: the `index_status_*` tests in this module.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IndexStatusResponse {
    /// The index this status describes.
    #[serde(default)]
    pub index_id: String,
    /// Per-lane pipeline state.
    #[serde(default)]
    pub stages: IndexStagesReport,
    /// The lanes that can actually answer a query right now, derived by
    /// trusty-search from `stages` (`bm25`, `literal`, `exact_match`, `vector`,
    /// `kg`). An EMPTY array means this index answers nothing.
    #[serde(default)]
    pub search_capabilities: Vec<String>,
    /// Present only when the durable corpus failed to open.
    #[serde(default)]
    pub corpus_open_failure: Option<CorpusOpenFailure>,
}

impl IndexStatusResponse {
    /// A fully-ready status for `index_id`.
    ///
    /// Why: every `SearchClient` stand-in — the test fakes across this crate and
    /// its integration tests — needs the healthy shape, and hand-rolling it per
    /// fake is how the fakes drift out of step with the wire type.
    /// What: all three lanes `ready`, the full capability list, no corpus
    /// failure.
    /// Test: used by the gate tests; asserted directly by
    /// `index_status_ready_helper_is_serving`.
    pub fn ready(index_id: impl Into<String>) -> Self {
        let ready = || IndexStageReport {
            status: "ready".to_string(),
            failure: None,
        };
        Self {
            index_id: index_id.into(),
            stages: IndexStagesReport {
                lexical: ready(),
                semantic: ready(),
                graph: ready(),
            },
            search_capabilities: ["bm25", "literal", "exact_match", "vector", "kg"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            corpus_open_failure: None,
        }
    }

    /// Classify what THIS index can do for a review right now (#6686).
    ///
    /// Why: the gate needs a per-index verdict in the same shape the health path
    /// produces, so `context_gate` folds both into one decision without
    /// re-deriving policy at the call site. The reason string it carries is what
    /// reaches the reader of a degraded review, so it must name the index and the
    /// lanes that actually failed rather than infer them.
    /// What: [`ServingState::Serving`] when no lane is `failed` and at least one
    /// search capability survives. Otherwise [`ServingState::Degraded`] with a
    /// reason naming the index, each failed lane (with trusty-search's own
    /// failure text where it supplied one), any corpus-open failure, and the
    /// capabilities that remain — `[none]` when the index answers nothing.
    /// [`ServingState::NotServing`] is never produced here: an index that exists
    /// but is broken is a degradation of one review, whereas an index that does
    /// not exist arrives as a `SearchClientError::Api { status: 404 }` and is
    /// handled by the caller as a hard skip (#6687).
    /// Test: `index_status_ready_helper_is_serving`,
    /// `index_status_all_lanes_failed_reports_no_capabilities`,
    /// `index_status_a_skipped_lane_is_not_a_failure`,
    /// `index_status_names_a_corpus_open_failure`,
    /// `index_status_empty_capabilities_degrade_without_a_failed_lane`.
    pub fn serving_state(&self) -> ServingState {
        let failed = self.stages.failed_lanes();
        if failed.is_empty() && !self.search_capabilities.is_empty() {
            return ServingState::Serving;
        }

        let id = &self.index_id;
        let mut clauses: Vec<String> = Vec::new();
        if !failed.is_empty() {
            clauses.push(format!("failed search lane(s): {}", failed.join(", ")));
        }
        if let Some(c) = self.corpus_open_failure.as_ref() {
            clauses.push(format!(
                "its durable corpus failed to open ({}{}) — trusty-search answers queries against \
                 it with an EMPTY result set and no error",
                c.kind,
                if c.transient { ", transient" } else { "" }
            ));
        }
        if self.search_capabilities.is_empty() {
            clauses.push(
                "it reports NO search capabilities, so every query against it returns nothing"
                    .to_string(),
            );
        }
        let capabilities = if self.search_capabilities.is_empty() {
            "none".to_string()
        } else {
            self.search_capabilities.join(", ")
        };
        ServingState::Degraded(format!(
            "index `{id}` is degraded — {}; surviving search capabilities: [{capabilities}]",
            clauses.join("; ")
        ))
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_status_ready_helper_is_serving() {
        assert_eq!(
            IndexStatusResponse::ready("trusty-tools").serving_state(),
            ServingState::Serving,
            "a fully-ready index must not degrade the review that queries it"
        );
    }

    /// The observed #6686 payload: the `workspace` index with every lane failed
    /// and `search_capabilities: []`.
    ///
    /// Why: the counter arithmetic this replaces announced "queries return
    /// LEXICAL results only" for exactly this index — a claim its own status
    /// payload contradicts. The reason must name the failed lanes and must never
    /// promise a lane that is dead.
    /// Test: this IS the test.
    #[test]
    fn index_status_all_lanes_failed_reports_no_capabilities() {
        let json = r#"{
            "index_id": "workspace",
            "search_capabilities": [],
            "stages": {
                "lexical": {"status": "failed", "failure": "walk budget refused"},
                "semantic": {"status": "failed"},
                "graph": {"status": "failed"}
            }
        }"#;
        let st: IndexStatusResponse = serde_json::from_str(json).expect("must parse");
        let ServingState::Degraded(reason) = st.serving_state() else {
            panic!(
                "all lanes failed must degrade, got {:?}",
                st.serving_state()
            );
        };
        assert!(
            reason.contains("workspace"),
            "the reason must name the index; got: {reason}"
        );
        for lane in ["lexical", "semantic", "graph"] {
            assert!(
                reason.contains(lane),
                "the reason must name the {lane} lane; got: {reason}"
            );
        }
        assert!(
            reason.contains("walk budget refused"),
            "trusty-search's own failure text must survive; got: {reason}"
        );
        assert!(
            !reason.contains("LEXICAL results only"),
            "#6686: the lexical lane failed too — the reason must not claim it answers; \
             got: {reason}"
        );
        assert!(
            reason.contains("[none]"),
            "the reason must state that nothing survives; got: {reason}"
        );
    }

    #[test]
    fn index_status_a_skipped_lane_is_not_a_failure() {
        // `--lexical-only` skips semantic and graph deliberately.
        let json = r#"{
            "index_id": "lex-only",
            "search_capabilities": ["bm25", "literal", "exact_match"],
            "stages": {
                "lexical": {"status": "ready"},
                "semantic": {"status": "skipped"},
                "graph": {"status": "skipped"}
            }
        }"#;
        let st: IndexStatusResponse = serde_json::from_str(json).expect("must parse");
        assert_eq!(
            st.serving_state(),
            ServingState::Serving,
            "a deliberate --lexical-only opt-out is not a fault"
        );
    }

    #[test]
    fn index_status_names_a_corpus_open_failure() {
        let json = r#"{
            "index_id": "stale",
            "search_capabilities": ["bm25"],
            "stages": {
                "lexical": {"status": "ready"},
                "semantic": {"status": "failed"},
                "graph": {"status": "ready"}
            },
            "corpus_open_failure": {"kind": "format_mismatch", "transient": false}
        }"#;
        let st: IndexStatusResponse = serde_json::from_str(json).expect("must parse");
        let ServingState::Degraded(reason) = st.serving_state() else {
            panic!("expected Degraded, got {:?}", st.serving_state());
        };
        assert!(
            reason.contains("format_mismatch") && reason.contains("EMPTY result set"),
            "the silent failure mode must be named with its consequence; got: {reason}"
        );
        assert!(
            reason.contains("bm25"),
            "the surviving capabilities must be stated; got: {reason}"
        );
    }

    /// An index with no failed lane but an empty capability array — warm-booted
    /// and still `pending` on every lane — answers nothing, so it degrades.
    #[test]
    fn index_status_empty_capabilities_degrade_without_a_failed_lane() {
        let json = r#"{
            "index_id": "cold",
            "search_capabilities": [],
            "stages": {"lexical": {"status": "pending"}}
        }"#;
        let st: IndexStatusResponse = serde_json::from_str(json).expect("must parse");
        let ServingState::Degraded(reason) = st.serving_state() else {
            panic!("an index that answers nothing must degrade");
        };
        assert!(reason.contains("NO search capabilities"), "got: {reason}");
    }

    /// Unknown fields must not break parsing — the live payload carries a dozen
    /// keys this type ignores.
    #[test]
    fn index_status_ignores_unknown_fields() {
        let json = r#"{
            "index_id": "x",
            "root_path": "/tmp/x",
            "chunk_count": 12,
            "status": "ready",
            "search_capabilities": ["bm25"],
            "stages": {"lexical": {"status": "ready", "chunks": 12}},
            "semantic_coverage": {"vectors_present": 3},
            "future_key": {"nested": true}
        }"#;
        let st: IndexStatusResponse =
            serde_json::from_str(json).expect("unknown fields must be discarded");
        assert_eq!(st.index_id, "x");
        assert_eq!(st.serving_state(), ServingState::Serving);
    }
}
