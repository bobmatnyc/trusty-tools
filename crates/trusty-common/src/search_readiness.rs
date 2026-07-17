//! Shared trusty-search index READINESS probe (issue #2784).
//!
//! Why: cold-indexing a large repo has a real semantic-warm-up window — the
//! lexical (BM25) lane comes up in seconds, but semantic embedding of a large
//! monorepo can stay `InProgress` for many minutes on `trusty-embedderd`. A
//! daily-driver session (a `tcode` task, a `trusty-mpm` session) that starts
//! working immediately searches BEFORE the semantic lane is ready and silently
//! gets lexical-only results with no signal that the index is still warming.
//! [`crate::search_index::ensure_project_indexed`] already *warms* the index
//! (find-or-create + freshness-gated reindex) at task start; this module adds
//! the missing half — *surfacing* the resulting readiness to the session so it
//! is not operating blind against a not-yet-ready index. Per the workspace
//! common-entry-point rule the primitive lives here, in one shared module, so
//! every caller reads the exact same `GET /indexes/{id}/status` contract.
//!
//! What: [`parse_readiness`] is a pure map from a `/status` JSON body to a small
//! [`IndexReadiness`] snapshot (lifecycle status + per-lane ready flags derived
//! from the daemon's `search_capabilities` array). [`probe_index_readiness`]
//! resolves the daemon address and canonical index id, does one short-timeout
//! blocking `GET` on a dedicated OS thread (safe to call from inside a tokio
//! runtime, mirroring [`crate::search_index`]'s HTTP helpers), and parses the
//! body — returning `None`, never an error, whenever the daemon is unreachable,
//! the id is not derivable, or the probe fails (fail-open by design, exactly
//! like the warming path). [`log_index_readiness`] is the session-facing
//! surface: it probes and emits ONE `tracing` line (to stderr, per the daemon
//! stdout-cleanliness rule) describing what a search will and won't yet find.
//!
//! Test: `parse_readiness_*`, `summary_*`, and
//! `probe_index_readiness_none_when_daemon_down` in the `tests` module below.

use std::path::Path;

/// The daemon `search_capabilities` token present once the lexical (BM25) lane
/// is queryable.
const CAP_LEXICAL: &str = "bm25";
/// The `search_capabilities` token present once the semantic (vector) lane is
/// queryable.
const CAP_SEMANTIC: &str = "vector";
/// The `search_capabilities` token present once the symbol-graph (KG) lane is
/// queryable.
const CAP_GRAPH: &str = "kg";

/// Coarse readiness snapshot of a trusty-search index (issue #2784).
///
/// Why: a session needs a single, cheap-to-render value that answers "can I
/// trust a semantic search yet, or am I still lexical-only?" without every
/// caller re-parsing the daemon's status JSON or knowing the
/// `search_capabilities` token spelling.
/// What: the canonical `index_id`, the daemon's coarse `lifecycle_status`
/// string (`created` → `walking` → `indexed_lexical` → `indexed_vector` →
/// `ready`, or `failed`), the durable `chunk_count`, and the three per-lane
/// ready flags derived from `search_capabilities`.
/// Test: constructed and asserted throughout the `tests` module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexReadiness {
    /// Canonical trusty-search index id the readiness pertains to.
    pub index_id: String,
    /// Daemon coarse lifecycle status string (back-compat `status` field).
    pub lifecycle_status: String,
    /// Durable chunk count reported by the daemon (0 while still empty).
    pub chunk_count: u64,
    /// Lexical / BM25 lane is queryable (`"bm25"` in `search_capabilities`).
    pub lexical_ready: bool,
    /// Semantic / vector lane is queryable (`"vector"` in `search_capabilities`).
    pub semantic_ready: bool,
    /// Symbol-graph / KG lane is queryable (`"kg"` in `search_capabilities`).
    pub graph_ready: bool,
}

impl IndexReadiness {
    /// Whether the semantic lane is up, i.e. hybrid/vector search is trustworthy.
    ///
    /// Why: the whole point of #2784 is the gap between "lexical ready" and
    /// "semantic ready"; this is the single predicate a caller checks to decide
    /// whether it is still operating in the lexical-only warm-up window.
    /// What: alias for `semantic_ready`, named for the caller's intent.
    /// Test: `summary_flags_semantic_pending_during_warmup`.
    pub fn semantic_search_ready(&self) -> bool {
        self.semantic_ready
    }

    /// One-line, human-readable readiness summary for a session log.
    ///
    /// Why: [`log_index_readiness`] needs a compact, information-dense string a
    /// developer reading the session's stderr can act on ("semantic still
    /// warming — expect lexical-only hits for a bit").
    /// What: names the index, the coarse lifecycle status, the chunk count, and
    /// the ready/pending state of each lane.
    /// Test: `summary_reports_all_lanes_ready`,
    /// `summary_flags_semantic_pending_during_warmup`.
    pub fn summary(&self) -> String {
        let lane = |ready: bool| if ready { "ready" } else { "pending" };
        format!(
            "trusty-search index '{}': {} ({} chunks) — lexical {}, semantic {}, graph {}",
            self.index_id,
            self.lifecycle_status,
            self.chunk_count,
            lane(self.lexical_ready),
            lane(self.semantic_ready),
            lane(self.graph_ready),
        )
    }
}

/// Pure map from a `GET /indexes/{id}/status` JSON body to an
/// [`IndexReadiness`] snapshot (issue #2784).
///
/// Why: keeping the parse pure makes the readiness rule unit-testable without a
/// live daemon — [`probe_index_readiness`] is otherwise all I/O — and pins the
/// `search_capabilities` token spelling in one place.
/// What: reads the coarse `status` string (defaulting to `"unknown"` when
/// absent/malformed), the durable `chunk_count` (defaulting to 0), and derives
/// the three per-lane flags from membership in the `search_capabilities` array.
/// Never fails: any missing/mistyped field degrades to a conservative
/// not-ready default rather than erroring.
/// Test: `parse_readiness_reads_capabilities_and_status`,
/// `parse_readiness_defaults_when_fields_absent`.
pub fn parse_readiness(index_id: &str, status: &serde_json::Value) -> IndexReadiness {
    let lifecycle_status = status
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let chunk_count = status
        .get("chunk_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let has_cap = |token: &str| {
        status
            .get("search_capabilities")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|caps| {
                caps.iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|c| c == token)
            })
    };
    IndexReadiness {
        index_id: index_id.to_string(),
        lifecycle_status,
        chunk_count,
        lexical_ready: has_cap(CAP_LEXICAL),
        semantic_ready: has_cap(CAP_SEMANTIC),
        graph_ready: has_cap(CAP_GRAPH),
    }
}

/// Best-effort probe of a project's trusty-search index readiness (issue #2784).
///
/// Why: a session wants to know, at start, whether the index it is about to
/// query is fully warm or still building — but must NEVER be blocked or aborted
/// by a slow/absent daemon (the same fail-open contract the warming path
/// honours). Returning `Option` (not `Result`) keeps every failure mode — no
/// daemon, undrivable id, HTTP error, non-2xx, unparseable body — a quiet
/// `None` at the call site.
/// What: resolves the canonical `(root, index_id)` the way
/// [`crate::search_index::ensure_project_indexed`] does (so this always targets
/// the same index the warming path created), discovers the daemon base URL, and
/// — on a dedicated OS thread with a ~1.5s overall / 750ms connect cap (safe
/// from inside a tokio runtime, mirroring the `search_index` HTTP helpers) —
/// does one `GET {base}/indexes/{id}/status`, parsing a 2xx body via
/// [`parse_readiness`]. `None` on any non-success, transport error, or when the
/// id/daemon can't be resolved.
/// Test: `probe_index_readiness_none_when_daemon_down` (daemon-down path); the
/// parse of a live body is covered by [`parse_readiness`]'s own tests.
pub fn probe_index_readiness(project_root: &Path) -> Option<IndexReadiness> {
    let root = crate::resolve_project_root(project_root);
    let index_id = crate::derive_index_id(&root);
    if index_id.trim().is_empty() {
        return None;
    }
    let base = crate::resolve_daemon_base_url("trusty-search")?;
    let status_url = format!("{base}/indexes/{index_id}/status");

    // Blocking reqwest inside a tokio runtime panics on runtime drop; run it on
    // a dedicated OS thread (joined here) exactly like search_index's helpers.
    let body: serde_json::Value = std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(1500))
            .connect_timeout(std::time::Duration::from_millis(750))
            .build()
            .ok()?;
        let resp = client.get(&status_url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<serde_json::Value>().ok()
    })
    .join()
    .ok()
    .flatten()?;

    Some(parse_readiness(&index_id, &body))
}

/// Probe a project's index readiness and surface it to the session as ONE log
/// line (issue #2784).
///
/// Why: this is the session-facing half of #2784. A daily-driver session that
/// starts querying during the semantic warm-up window otherwise gets
/// lexical-only results with no signal; a single readiness line on stderr tells
/// the developer what a search will and won't yet find, so a not-yet-ready
/// index is never silent. Fail-open: a `None` probe (no daemon, etc.) logs
/// nothing — surfacing readiness must never itself become noise or a blocker.
/// What: calls [`probe_index_readiness`]; on `Some`, logs [`IndexReadiness::summary`]
/// at `info` when the semantic lane is ready and at `warn` while it is still
/// warming (so the warm-up window is the one that stands out). Writes via
/// `tracing`, which the daemons route to stderr, keeping stdout clean for MCP
/// JSON-RPC framing.
/// Test: side-effect-only (emits a log, no return to assert); its logic is
/// [`probe_index_readiness`] + [`log_readiness`], both unit-tested.
pub fn log_index_readiness(project_root: &Path) {
    if let Some(readiness) = probe_index_readiness(project_root) {
        log_readiness(&readiness);
    }
}

/// Emit the session-facing readiness log line for an ALREADY-probed
/// [`IndexReadiness`].
///
/// Why: [`log_index_readiness`] probes and logs in one step, which is all a
/// log-only caller needs — but a caller that must ALSO forward the readiness
/// somewhere else (e.g. `trusty-code` publishing it as a daemon API event for
/// its UI) would otherwise have to probe a second time, doubling the HTTP cost
/// and risking the two observations disagreeing about a lane that flipped
/// between them. Splitting the log step out lets such a caller probe ONCE via
/// [`probe_index_readiness`] and then both log and forward the same snapshot.
/// What: logs [`IndexReadiness::summary`] at `info` when the semantic lane is
/// ready and at `warn` while it is still warming (so the warm-up window is the
/// one that stands out), via `tracing` — which the daemons route to stderr,
/// keeping stdout clean for MCP JSON-RPC framing. Pure side effect; never
/// fails.
/// Test: `log_readiness_does_not_panic_for_either_lane_state`; the message
/// content it formats is covered by [`IndexReadiness::summary`]'s own tests.
pub fn log_readiness(readiness: &IndexReadiness) {
    if readiness.semantic_search_ready() {
        tracing::info!("{}", readiness.summary());
    } else {
        tracing::warn!(
            "{} — semantic search still warming; expect lexical-only results until it is ready",
            readiness.summary()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_readiness_reads_capabilities_and_status() {
        // A mid-warm-up body: lexical up, semantic still building.
        let body = json!({
            "status": "indexed_lexical",
            "chunk_count": 4096,
            "search_capabilities": ["bm25", "literal", "exact_match"],
        });
        let r = parse_readiness("trusty-tools", &body);
        assert_eq!(r.index_id, "trusty-tools");
        assert_eq!(r.lifecycle_status, "indexed_lexical");
        assert_eq!(r.chunk_count, 4096);
        assert!(r.lexical_ready, "bm25 present ⇒ lexical ready");
        assert!(!r.semantic_ready, "no vector cap ⇒ semantic not ready");
        assert!(!r.graph_ready);
    }

    #[test]
    fn parse_readiness_all_lanes_ready() {
        let body = json!({
            "status": "ready",
            "chunk_count": 50_000,
            "search_capabilities": ["bm25", "literal", "exact_match", "vector", "kg"],
        });
        let r = parse_readiness("big-repo", &body);
        assert!(r.lexical_ready && r.semantic_ready && r.graph_ready);
        assert!(r.semantic_search_ready());
    }

    #[test]
    fn parse_readiness_defaults_when_fields_absent() {
        // Empty object: every field degrades to a conservative not-ready value.
        let r = parse_readiness("x", &json!({}));
        assert_eq!(r.lifecycle_status, "unknown");
        assert_eq!(r.chunk_count, 0);
        assert!(!r.lexical_ready && !r.semantic_ready && !r.graph_ready);
    }

    #[test]
    fn parse_readiness_ignores_malformed_capabilities() {
        // A non-array `search_capabilities` must not panic or falsely report ready.
        let r = parse_readiness("x", &json!({ "search_capabilities": "bm25" }));
        assert!(!r.lexical_ready);
    }

    #[test]
    fn summary_reports_all_lanes_ready() {
        let r = IndexReadiness {
            index_id: "repo".into(),
            lifecycle_status: "ready".into(),
            chunk_count: 10,
            lexical_ready: true,
            semantic_ready: true,
            graph_ready: true,
        };
        let s = r.summary();
        assert!(s.contains("'repo'"));
        assert!(s.contains("ready (10 chunks)"));
        assert!(s.contains("semantic ready"));
    }

    #[test]
    fn summary_flags_semantic_pending_during_warmup() {
        let r = IndexReadiness {
            index_id: "repo".into(),
            lifecycle_status: "indexed_lexical".into(),
            chunk_count: 7,
            lexical_ready: true,
            semantic_ready: false,
            graph_ready: false,
        };
        assert!(!r.semantic_search_ready());
        let s = r.summary();
        assert!(s.contains("lexical ready"));
        assert!(s.contains("semantic pending"));
    }

    /// `log_readiness` is pure side effect; assert only that BOTH lane states
    /// (the `info` ready branch and the `warn` warm-up branch) execute without
    /// panicking. The message content is `summary`'s contract, tested above.
    #[test]
    fn log_readiness_does_not_panic_for_either_lane_state() {
        let mut r = IndexReadiness {
            index_id: "repo".into(),
            lifecycle_status: "indexed_lexical".into(),
            chunk_count: 7,
            lexical_ready: true,
            semantic_ready: false,
            graph_ready: false,
        };
        log_readiness(&r); // warm-up branch
        r.semantic_ready = true;
        log_readiness(&r); // ready branch
    }

    #[test]
    fn probe_index_readiness_none_for_root() {
        // The filesystem root derives an empty id ⇒ no probe, `None`.
        assert_eq!(probe_index_readiness(Path::new("/")), None);
    }

    #[test]
    fn probe_index_readiness_none_when_daemon_down() {
        // Point the data dir at an empty temp dir so `resolve_daemon_base_url`
        // finds no address file ⇒ daemon-down path returns `None`, never an
        // error. `ENV_LOCK` serialises the process-global override against
        // sibling env-mutating tests, matching `search_index`'s tests.
        let _guard = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let data_dir = std::env::temp_dir().join(format!("trusty-readiness-data-{pid}-{nanos}"));
        std::fs::create_dir_all(&data_dir).unwrap();
        // SAFETY: guarded by ENV_LOCK; removed below before returning.
        unsafe {
            std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &data_dir);
        }

        let project = data_dir.join("proj");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let readiness = probe_index_readiness(&project);

        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        }
        let _ = std::fs::remove_dir_all(&data_dir);

        assert_eq!(readiness, None, "daemon down ⇒ None, never an error");
    }
}
