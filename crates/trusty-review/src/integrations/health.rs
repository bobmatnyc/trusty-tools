//! Health-response types for the trusty-search `/health` endpoint.
//!
//! Why: trusty-search v0.22+ changed the `embedder` field from a boolean to
//! the string `"ready"`.  The old `bool` field caused a hard deserialisation
//! failure, making every review on current trusty-search appear to use an
//! unreachable daemon (closes #628).
//!
//! What: defines `EmbedderState` (tolerates both bool and string forms) and
//! `HealthResponse` (the full `/health` wire type).  Unknown JSON fields are
//! silently discarded so future trusty-search additions do not re-break parsing.
//!
//! Test: `health_response_*` tests below; no live daemon required.

use serde::{Deserialize, Deserializer, Serialize};

// ─── EmbedderState ────────────────────────────────────────────────────────────

/// Tolerant deserialiser for the `embedder` field of `GET /health`.
///
/// Why: trusty-search v0.21 returned a bool (`true`/`false`); v0.22+ returns a
/// string (`"ready"`, `"loading"`, …).  Deserialising as a strict `bool` causes
/// a hard parse error on v0.22+, making every review appear to run against an
/// unreachable daemon (closes #628).
/// What: an untagged enum that accepts either JSON form and converts to a single
/// `ready: bool` for callers.  Any string other than `"ready"` (case-insensitive)
/// is treated as not-ready; `false` is not-ready; `true` is ready.
/// Test: `embedder_state_bool_true`, `embedder_state_string_ready`,
/// `embedder_state_string_loading`, `embedder_state_bool_false`.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EmbedderState {
    /// JSON boolean form (trusty-search ≤ v0.21).
    Bool(bool),
    /// JSON string form (trusty-search v0.22+, e.g. `"ready"`, `"loading"`).
    Str(String),
}

impl EmbedderState {
    /// Returns `true` when the embedder is ready to serve requests.
    ///
    /// Why: callers need a single boolean gate; this centralises the mapping so
    /// it is easy to update if trusty-search introduces new status strings.
    /// What: `Bool(true)` → `true`; `Str(s)` where `s.eq_ignore_ascii_case("ready")` → `true`;
    /// everything else → `false`.
    /// Test: `embedder_state_*` tests in this module.
    pub fn is_ready(&self) -> bool {
        match self {
            EmbedderState::Bool(b) => *b,
            EmbedderState::Str(s) => s.eq_ignore_ascii_case("ready"),
        }
    }
}

impl<'de> Deserialize<'de> for EmbedderState {
    /// Custom deserialiser that accepts either a JSON bool or a JSON string.
    ///
    /// Why: the standard `#[serde(untagged)]` derive on an enum containing
    /// `bool` and `String` fields works correctly for deserialisation from
    /// JSON — serde tries each variant in order: bool first, then String.
    /// This manual implementation is provided for clarity and to allow a unit
    /// test to verify the exact mapping without any macro magic.
    /// What: tries to deserialise a bool first; falls back to a string.
    /// Test: `embedder_state_*` tests in this module.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Use a helper that can hold either form.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bool(b) => Ok(EmbedderState::Bool(b)),
            Raw::Str(s) => Ok(EmbedderState::Str(s)),
        }
    }
}

impl Default for EmbedderState {
    /// Default is not-ready (`Bool(false)`) — conservative assumption when the
    /// field is absent from the JSON response.
    ///
    /// Why: `#[serde(default)]` on `HealthResponse.embedder` requires `Default`.
    /// A missing field should be treated as not-ready rather than ready.
    /// What: returns `EmbedderState::Bool(false)`.
    /// Test: `health_response_missing_embedder_defaults_to_not_ready`.
    fn default() -> Self {
        EmbedderState::Bool(false)
    }
}

// ─── HealthResponse ───────────────────────────────────────────────────────────

/// Response from `GET /health` on trusty-search.
///
/// Why: the pipeline checks health before issuing a search to give a clear
/// "service unavailable" error rather than a confusing transport failure.
/// Tolerates both the old bool `embedder` (≤ v0.21) and the new string form
/// (v0.22+: `"ready"`, `"loading"`, etc.) so parsing never fails due to a
/// field-type mismatch (closes #628).
/// What: `status == "ok"` is the primary health gate; `embedder.is_ready()`
/// confirms the embedding model is loaded.  Unknown extra fields are discarded
/// (`#[serde(default)]` + no `deny_unknown_fields`) so future additions to the
/// trusty-search health payload don't break this consumer.
/// Test: `health_response_*` tests in this module cover all four representative
/// inputs specified in #628.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthResponse {
    /// `"ok"` when healthy, `"degraded"` when serving but with a known
    /// capability gap (see `warmboot_summary` / #3693), anything else when
    /// not serving.
    pub status: String,
    /// Whether the embedding model is loaded and ready.  Tolerates both
    /// JSON bool (`true`/`false`) and JSON string (`"ready"`, `"loading"`, …).
    #[serde(default)]
    pub embedder: EmbedderState,
    /// Warm-boot health summary (issues #3693, #3706, #4086).
    ///
    /// Why: since trusty-search #3706 the top-level `status` is `"degraded"`
    /// whenever ANY warm-boot condition tripped — a benign auto-disabled file
    /// watcher on a network-mounted root, a corpus that failed to open, a
    /// warm-boot scan timeout, a TCC/FDA denial, or mass index loss. The status
    /// string alone therefore cannot distinguish "fine, one unrelated repo's
    /// index did not load" from "queries against index X silently return
    /// nothing". [`HealthResponse::serving_state`] reads the individual counters
    /// in this summary to build an actionable reason instead of guessing from
    /// the status string. Absent on older trusty-search versions or a partial
    /// response, which is reported as degraded-with-unknown-cause rather than as
    /// an outage — the daemon did answer the probe.
    /// Test: `health_response_degraded_watcher_only_is_serving`,
    /// `health_response_live_warmboot_degraded_payload_is_reachable`,
    /// `health_response_degraded_missing_summary_is_degraded_not_fatal`.
    #[serde(default)]
    pub warmboot_summary: Option<WarmBootSummary>,
}

/// Mirror of trusty-search's warm-boot health summary (issues #3693, #4086).
///
/// Why: the aggregate `warm_boot_degraded` flag alone is not actionable. It is
/// an OR over four structurally different conditions (TCC/FDA denial, warm-boot
/// scan timeout, corpus-open failure, and mass index loss), and since
/// trusty-search #3706 folded that whole aggregate into the top-level `status`
/// field, ANY one of them makes trusty-search report `status: "degraded"`.
/// Branching on the aggregate alone therefore cannot tell a daemon that is
/// silently answering queries wrong (corpus-open failure) from one that merely
/// left some unrelated repo's index unloaded (scan timeout) — and reading it as
/// "not serving" reported a live, query-answering daemon as unreachable
/// (#4086). Mirroring the individual counters lets `serving_state` explain
/// WHICH gap exists, in a message an operator can act on.
/// What: the individual warm-boot counters plus the aggregate flag. Every field
/// is `#[serde(default)]` and unknown fields are discarded (no
/// `deny_unknown_fields`) so a trusty-search-side addition never breaks parsing
/// here.
/// Test: see `HealthResponse` test list above; `warm_boot_summary_wire_shape_is_pinned`
/// below pins the exact field names this type deserialises against.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WarmBootSummary {
    /// Indexes successfully restored into the registry at warm boot.
    #[serde(default)]
    pub indexes_loaded: u32,
    /// Indexes skipped at warm boot because macOS TCC/FDA denied access to
    /// their root. They are absent from the registry until the next boot.
    #[serde(default)]
    pub indexes_skipped_tcc: u32,
    /// Indexes skipped at warm boot because their restore exceeded the
    /// per-index deadline. Like the TCC case they are absent from the registry
    /// entirely (not parked for lazy load), so a query naming one of them 404s
    /// loudly and `search_all` simply does not fan out to it.
    #[serde(default)]
    pub indexes_skipped_timeout: u32,
    /// Registered indexes whose restore was recorded as failed.
    #[serde(default)]
    pub indexes_failed: u32,
    /// Registered indexes whose durable corpus failed to open. This is the one
    /// genuinely SILENT failure mode: trusty-search keeps serving them and
    /// returns `200 OK` with an EMPTY result set rather than an error, so a
    /// review against such an index would get no context and no signal that
    /// anything was wrong.
    ///
    /// trusty-search #5927 narrowed this key to the fact its name states. It
    /// used to count any index with a failed lexical, semantic, or graph lane
    /// as well, so this doc comment described a superset of what the number
    /// meant. That superset now arrives as [`Self::indexes_stage_failed`].
    #[serde(default)]
    pub indexes_corpus_failed: u32,
    /// Registered indexes with at least one failed search lane — lexical,
    /// semantic, or graph (trusty-search #5927).
    ///
    /// A strict superset of [`Self::indexes_corpus_failed`]: a corpus-open
    /// failure fails every lane. The excess over that count is indexes whose
    /// corpus is fine but whose embed or graph lane died, which serve partial
    /// results rather than empty ones.
    ///
    /// Reading it is what keeps `degraded_reason` from regressing across the
    /// #5927 split. Before #5927 a lane failure arrived under the corpus key
    /// and produced a clause; without this field it would produce none, and a
    /// reason-less `"degraded"` is classified `Serving` — turning a real
    /// capability gap into silence on the review that ran.
    #[serde(default)]
    pub indexes_stage_failed: u32,
    /// trusty-search's own aggregate "my warm boot was broken" flag — the OR of
    /// the counters above plus mass index loss.
    ///
    /// Kept on the wire mirror because it is part of the pinned payload shape
    /// and is re-serialised by consumers of this type, but deliberately NOT
    /// branched on: reading the aggregate is exactly what made a live daemon
    /// report as unreachable (#4086). `serving_state` reads the individual
    /// counters instead, so a future trusty-search rename of this key degrades
    /// to a `false` default without changing any decision — the counters, and
    /// `warm_boot_summary_wire_shape_is_pinned`, are the real guards.
    #[serde(default)]
    pub warm_boot_degraded: bool,
}

impl HealthResponse {
    /// Returns `true` when the daemon is healthy and the embedder is loaded.
    ///
    /// Why: kept for callers that want the strict "fully nominal" gate
    /// (e.g. surfacing a friendly status string), as distinct from
    /// `is_serving`'s "can I get a real review out of it" gate.
    /// What: checks `status == "ok"` (primary gate) AND `embedder.is_ready()`.
    /// Test: `health_response_is_healthy`, `health_response_embedder_not_ready`.
    pub fn is_healthy(&self) -> bool {
        self.status == "ok" && self.embedder.is_ready()
    }

    /// Classify what trusty-search can actually do for a review right now
    /// (issues #3693, #4086).
    ///
    /// Why: reachability and full health are different questions, and folding
    /// them into one boolean produced a flatly false report. trusty-search
    /// #3706 folded the whole `warm_boot_degraded` aggregate into the top-level
    /// `status`, so a daemon that merely skipped some OTHER repo's index on a
    /// warm-boot scan timeout now answers `status: "degraded"`. The previous
    /// `is_serving` read that as "not serving", which `probe_deps` in turn
    /// reported as `reachable: false, state: "unreachable"` — for a daemon that
    /// was up, embedder-ready, and answering queries. Operators reasonably read
    /// "unreachable" as "the daemon is down" and went looking for a dead
    /// process that did not exist (#4086). The three outcomes here keep the
    /// genuinely-fatal cases fatal while making a partial capability gap
    /// visible and explainable instead of masquerading as an outage.
    /// What: [`ServingState::NotServing`] when the embedder is not ready (no
    /// semantic search is possible at all) or the status is neither `"ok"` nor
    /// `"degraded"` (`"starting"`, `"error"`, …). [`ServingState::Serving`]
    /// when `status == "ok"`. Otherwise [`ServingState::Degraded`] carrying a
    /// human-readable reason built from the individual warm-boot counters — a
    /// `"degraded"` daemon is still answering queries, so the caller proceeds
    /// but must stamp the reason onto its output rather than swallow it.
    ///
    /// Deliberate policy change vs. the pre-#4086 behaviour: a warm-boot gap is
    /// no longer a blanket refusal. `indexes_corpus_failed` (the one silent
    /// failure mode — trusty-search answers `200 OK` with an empty result set
    /// for such an index) and `indexes_skipped_*` (indexes absent from the
    /// registry, where a direct query 404s loudly) are BOTH per-index
    /// conditions, and `/health` does not say which indexes they apply to. One
    /// unrelated repo's bad corpus must not silently cancel every review on the
    /// host; it must instead be reported on the review that ran. Closing the
    /// remaining gap properly needs a per-index status probe against the index
    /// the review actually uses — see the follow-up filed as #6686.
    /// Test: `health_response_degraded_watcher_only_is_serving`,
    /// `health_response_live_warmboot_degraded_payload_is_reachable`,
    /// `health_response_degraded_reason_names_corpus_failures`,
    /// `health_response_degraded_missing_summary_is_degraded_not_fatal`,
    /// `health_response_ok_is_serving`,
    /// `health_response_ok_embedder_not_ready_is_not_serving`,
    /// `health_response_other_status_is_not_serving`.
    pub fn serving_state(&self) -> ServingState {
        if !self.embedder.is_ready() {
            return ServingState::NotServing(
                "trusty-search embedder is not ready — semantic code context is unavailable"
                    .to_string(),
            );
        }
        match self.status.as_str() {
            "ok" => ServingState::Serving,
            // A `"degraded"` status with every warm-boot counter clean is the
            // benign network-mount watcher disable of #3408/#3693: indexes stay
            // fully queryable, so this is `Serving`, exactly as #3693 intended.
            "degraded" => match self.degraded_reason() {
                Some(reason) => ServingState::Degraded(reason),
                None => ServingState::Serving,
            },
            other => ServingState::NotServing(format!(
                "trusty-search reports status {other:?} — not serving queries"
            )),
        }
    }

    /// Answer the ONE question `/health` is qualified to answer: is this daemon
    /// reachable and serving queries (#6686)?
    ///
    /// Why: `/health` counts registry handles and discards index ids
    /// (`crates/trusty-search/src/service/server/health.rs`), so its warm-boot
    /// counters describe the HOST, never the index a given review queries. The
    /// required-context gate used to branch on them and therefore degraded every
    /// review on a host where any unrelated index had failed — the review whose
    /// own index was healthy included (#6686). The per-index probe
    /// [`crate::integrations::index_status::IndexStatusResponse::serving_state`]
    /// answers the index-scoped question; this method answers the host-scoped
    /// one and reads no counter to do it.
    /// What: [`ServingState::NotServing`] when the embedder is not ready (no
    /// semantic context is possible at all) or the status is neither `"ok"` nor
    /// `"degraded"` (`"starting"`, `"error"`, …). Otherwise
    /// [`ServingState::Serving`] — a `"degraded"` daemon is answering queries,
    /// and WHAT it lost is the per-index probe's question, not this one.
    /// [`ServingState::Degraded`] is never returned.
    /// Test: `reachability_state_ignores_warm_boot_counters`,
    /// `reachability_state_embedder_not_ready_is_not_serving`,
    /// `reachability_state_other_status_is_not_serving`.
    pub fn reachability_state(&self) -> ServingState {
        if !self.embedder.is_ready() {
            return ServingState::NotServing(
                "trusty-search embedder is not ready — semantic code context is unavailable"
                    .to_string(),
            );
        }
        match self.status.as_str() {
            "ok" | "degraded" => ServingState::Serving,
            other => ServingState::NotServing(format!(
                "trusty-search reports status {other:?} — not serving queries"
            )),
        }
    }

    /// Build the operator-facing explanation for a `status: "degraded"` daemon,
    /// or `None` when the degradation does not affect query results.
    ///
    /// Why: "degraded" on its own tells an operator nothing about whether their
    /// review lost context. Naming the specific counters — and what each one
    /// means for query results — is the difference between an actionable notice
    /// and noise, and it is what gets stamped into a degraded review body.
    /// What: `None` when every warm-boot counter is clean (the benign
    /// network-mount watcher disable of #3408/#3693 — indexes remain fully
    /// queryable, so there is nothing to report). Otherwise a `; `-joined clause
    /// list built from the non-zero counters, each annotated with its query-time
    /// consequence. A missing summary yields a generic message rather than
    /// `None`: the cause is unknown, which is itself worth saying.
    /// Test: `health_response_degraded_reason_names_corpus_failures`,
    /// `health_response_degraded_missing_summary_is_degraded_not_fatal`,
    /// `health_response_degraded_watcher_only_is_serving`.
    fn degraded_reason(&self) -> Option<String> {
        let Some(w) = self.warmboot_summary.as_ref() else {
            return Some(
                "trusty-search reports status \"degraded\" but sent no warm-boot summary \
                 (older trusty-search or partial response) — code context may be incomplete"
                    .to_string(),
            );
        };
        let mut clauses: Vec<String> = Vec::new();
        if w.indexes_corpus_failed > 0 {
            clauses.push(format!(
                "{} index(es) failed to open their corpus — trusty-search answers queries \
                 against those indexes with an EMPTY result set and no error",
                w.indexes_corpus_failed
            ));
        }
        // #6686: this clause used to subtract `indexes_corpus_failed` from
        // `indexes_stage_failed` and announce that the remainder "return LEXICAL
        // results only". Two host-wide totals cannot support that claim, and it
        // was flatly wrong for the index that produced #6686 — `workspace` had
        // its lexical lane failed too and reported `search_capabilities: []`.
        // The counter says how MANY indexes have a dead lane and nothing more;
        // which index and which lane is what `GET /indexes/{id}/status` answers,
        // and that probe — not this arithmetic — is now what the review gate
        // decides on.
        if w.indexes_stage_failed > 0 {
            clauses.push(format!(
                "{} index(es) have at least one failed search lane, lexical included (a superset \
                 of any corpus failures above) — /health does not say which index or which lane, \
                 so read GET /indexes/{{id}}/status for the index in question",
                w.indexes_stage_failed
            ));
        }
        if w.indexes_failed > 0 {
            clauses.push(format!("{} index(es) failed to restore", w.indexes_failed));
        }
        if w.indexes_skipped_timeout > 0 {
            clauses.push(format!(
                "{} index(es) were skipped on a warm-boot scan timeout and are absent from the \
                 registry",
                w.indexes_skipped_timeout
            ));
        }
        if w.indexes_skipped_tcc > 0 {
            clauses.push(format!(
                "{} index(es) were skipped because macOS Full Disk Access was denied",
                w.indexes_skipped_tcc
            ));
        }
        if clauses.is_empty() {
            // `status: "degraded"` with every warm-boot counter clean is the
            // benign network-mount watcher disable of #3408/#3693 — indexes stay
            // fully queryable, so there is nothing to warn a reviewer about.
            return None;
        }
        Some(format!(
            "serving but degraded ({} of {} index(es) loaded): {}",
            w.indexes_loaded,
            w.indexes_loaded + w.indexes_skipped_timeout + w.indexes_skipped_tcc,
            clauses.join("; ")
        ))
    }

    /// Returns `true` when trusty-search can serve queries at all.
    ///
    /// Why: several call sites only need the yes/no gate and do not care about
    /// the reason; keeping the boolean means they do not each re-match on
    /// [`ServingState`].
    /// What: `!matches!(self.serving_state(), ServingState::NotServing(_))` —
    /// note that a `Degraded` daemon counts as serving, because it is answering
    /// queries. Callers that must report or stamp the degradation use
    /// [`HealthResponse::serving_state`] instead.
    /// Test: `health_response_live_warmboot_degraded_payload_is_reachable`,
    /// `health_response_ok_embedder_not_ready_is_not_serving`.
    pub fn is_serving(&self) -> bool {
        !matches!(self.serving_state(), ServingState::NotServing(_))
    }
}

/// What trusty-search can do for a review right now (issue #4086).
///
/// Why: the pre-#4086 code had one boolean for two questions — "is the daemon
/// answering?" and "is it fully healthy?" — and reported the answer to the
/// second under the name of the first. Three explicit states make the middle
/// case (up and answering, with a named capability gap) representable, so it can
/// be reported honestly instead of being rounded down to "unreachable".
/// What: `Serving` (fully nominal), `Degraded(reason)` (answering queries, with
/// an operator-facing explanation that MUST be surfaced, never swallowed), and
/// `NotServing(reason)` (cannot produce code context at all).
/// Test: the `health_response_*` tests below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingState {
    /// Fully nominal — `status: "ok"` and the embedder is ready.
    Serving,
    /// Answering queries, but with a named capability gap that the caller must
    /// surface on any output derived from this daemon.
    Degraded(String),
    /// Cannot supply code context at all.
    NotServing(String),
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── EmbedderState mapping ─────────────────────────────────────────────────

    #[test]
    fn embedder_state_bool_true_is_ready() {
        let s = EmbedderState::Bool(true);
        assert!(s.is_ready(), "Bool(true) must be ready");
    }

    #[test]
    fn embedder_state_bool_false_is_not_ready() {
        let s = EmbedderState::Bool(false);
        assert!(!s.is_ready(), "Bool(false) must not be ready");
    }

    #[test]
    fn embedder_state_string_ready_is_ready() {
        let s = EmbedderState::Str("ready".to_string());
        assert!(s.is_ready(), r#"Str("ready") must be ready"#);
    }

    #[test]
    fn embedder_state_string_ready_case_insensitive() {
        // Verify case-insensitive match: "READY", "Ready", "rEaDy".
        for variant in ["READY", "Ready", "rEaDy"] {
            let s = EmbedderState::Str(variant.to_string());
            assert!(
                s.is_ready(),
                "Str({variant:?}) must be ready (case-insensitive)"
            );
        }
    }

    #[test]
    fn embedder_state_string_loading_is_not_ready() {
        let s = EmbedderState::Str("loading".to_string());
        assert!(!s.is_ready(), r#"Str("loading") must not be ready"#);
    }

    #[test]
    fn embedder_state_string_empty_is_not_ready() {
        let s = EmbedderState::Str(String::new());
        assert!(!s.is_ready(), "Str(\"\") must not be ready");
    }

    #[test]
    fn embedder_state_default_is_not_ready() {
        let s = EmbedderState::default();
        assert!(!s.is_ready(), "Default EmbedderState must not be ready");
    }

    // ── Deserialisation — representative /health bodies ────────────────────────

    /// Regression: trusty-search v0.22+ returns embedder as string "ready".
    /// This was the hard parse error that triggered #628.
    #[test]
    fn health_response_embedder_string_ready_is_healthy() {
        let json = r#"{"status":"ok","version":"0.22.1","indexes":132,"uptime_secs":3600,"embedder":"ready"}"#;
        let resp: HealthResponse =
            serde_json::from_str(json).expect("must parse: this was the failing case in #628");
        assert!(
            resp.is_healthy(),
            "embedder=string:\"ready\" + status=ok must be healthy"
        );
    }

    /// Back-compat: trusty-search ≤ v0.21 returns embedder as bool true.
    #[test]
    fn health_response_embedder_bool_true_is_healthy() {
        let json = r#"{"status":"ok","embedder":true}"#;
        let resp: HealthResponse = serde_json::from_str(json).expect("must parse: bool true form");
        assert!(
            resp.is_healthy(),
            "embedder=bool:true + status=ok must be healthy"
        );
    }

    /// embedder=string:"loading" — parses successfully, but not healthy.
    #[test]
    fn health_response_embedder_string_loading_parses_not_healthy() {
        let json = r#"{"status":"ok","embedder":"loading"}"#;
        let resp: HealthResponse = serde_json::from_str(json).expect("must parse without error");
        assert!(
            !resp.is_healthy(),
            "embedder=string:\"loading\" must parse OK but report not healthy"
        );
    }

    /// embedder=bool:false — parses successfully, but not healthy.
    #[test]
    fn health_response_embedder_bool_false_parses_not_healthy() {
        let json = r#"{"status":"ok","embedder":false}"#;
        let resp: HealthResponse = serde_json::from_str(json).expect("must parse without error");
        assert!(
            !resp.is_healthy(),
            "embedder=bool:false must parse OK but report not healthy"
        );
    }

    /// Extra unknown fields must not cause a parse failure.
    #[test]
    fn health_response_extra_fields_ignored() {
        let json = r#"{
            "status": "ok",
            "embedder": "ready",
            "version": "0.22.1",
            "indexes": 132,
            "uptime_secs": 3600,
            "unknown_future_field": {"nested": true}
        }"#;
        let resp: HealthResponse =
            serde_json::from_str(json).expect("extra fields must be silently ignored");
        assert!(resp.is_healthy());
    }

    /// Missing `embedder` field defaults to not-ready; status=ok alone is not enough.
    #[test]
    fn health_response_missing_embedder_defaults_to_not_ready() {
        let json = r#"{"status":"ok"}"#;
        let resp: HealthResponse =
            serde_json::from_str(json).expect("missing embedder must default gracefully");
        assert!(
            !resp.is_healthy(),
            "missing embedder field must default to not-ready"
        );
    }

    /// status != "ok" means unhealthy regardless of embedder value.
    #[test]
    fn health_response_bad_status_is_unhealthy() {
        let json = r#"{"status":"starting","embedder":"ready"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(
            !resp.is_healthy(),
            "status != ok must be unhealthy even if embedder is ready"
        );
    }

    // ── is_serving (#3693) ──────────────────────────────────────────────────

    /// status=="ok" + embedder ready must be serving (unchanged fast path).
    #[test]
    fn health_response_ok_is_serving() {
        let json = r#"{"status":"ok","embedder":"ready"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(resp.is_serving(), "status=ok + embedder ready must serve");
    }

    /// status=="ok" but embedder not ready must NOT be serving — an embedder
    /// still initializing/dead cannot supply real code context even if the
    /// daemon considers itself otherwise `"ok"`.
    #[test]
    fn health_response_ok_embedder_not_ready_is_not_serving() {
        let json = r#"{"status":"ok","embedder":"loading"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(
            !resp.is_serving(),
            "embedder not ready must not serve even if status=ok"
        );
    }

    /// The exact #3693 scenario: trusty-search 0.38.1 on an EFS/NFS mount
    /// reports `status: "degraded"` purely because the file watcher was
    /// auto-disabled — search itself is 100% functional. Must be serving.
    #[test]
    fn health_response_degraded_watcher_only_is_serving() {
        let json = r#"{
            "status": "degraded",
            "embedder": "ready",
            "warmboot_summary": {"warm_boot_degraded": false}
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.serving_state(),
            ServingState::Serving,
            "degraded solely due to watcher-network-degraded must serve CLEANLY (#3693) — a clean \
             warm boot leaves nothing for a reviewer to act on, so it must not raise a caveat"
        );
    }

    /// REGRESSION (#4086): the VERBATIM `/health` payload captured from the
    /// live daemon that trusty-review reported as `state: "unreachable"`.
    ///
    /// Why: this exact daemon was up, embedder-ready, and answering queries
    /// (`uptime_secs: 3220`, 20 indexes loaded, `indexes_failed: 0`) while
    /// `review_health` reported `trusty_search: {reachable: false, state:
    /// "unreachable"}`. That word sent an operator hunting for a dead process
    /// that did not exist. A daemon that returns a parseable health body is
    /// reachable, full stop; a warm-boot gap is reported as degraded WITH a
    /// reason, never as an outage.
    /// Test: this test.
    #[test]
    fn health_response_live_warmboot_degraded_payload_is_reachable() {
        // Captured verbatim from the live trusty-search 0.39.1 daemon.
        let json = r#"{
            "status": "degraded",
            "version": "0.39.1",
            "uptime_secs": 3220,
            "embedder": "ready",
            "embedder_last_ok_secs_ago": 0,
            "embedder_recent_timeout_count": 0,
            "indexes": 20,
            "warmboot_summary": {
                "indexes_corpus_failed": 3,
                "indexes_failed": 0,
                "indexes_lazy": 0,
                "indexes_loaded": 20,
                "indexes_skipped_timeout": 11,
                "indexes_skipped_tcc": 0,
                "warm_boot_degraded": true
            }
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(
            resp.is_serving(),
            "a live, embedder-ready, query-answering daemon must never be classified as \
             not-serving just because warm boot left some indexes behind (#4086)"
        );
        assert!(
            matches!(resp.serving_state(), ServingState::Degraded(_)),
            "the warm-boot gap must still be reported — as Degraded, not swallowed"
        );
    }

    /// The degraded reason must NAME the silent failure mode, not just say
    /// "degraded": an index whose corpus failed to open answers `200 OK` with
    /// an empty result set, so a review against it loses context invisibly.
    #[test]
    fn health_response_degraded_reason_names_corpus_failures() {
        let json = r#"{
            "status": "degraded",
            "embedder": "ready",
            "warmboot_summary": {
                "indexes_loaded": 20,
                "indexes_skipped_timeout": 11,
                "indexes_corpus_failed": 3,
                "warm_boot_degraded": true
            }
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        let ServingState::Degraded(reason) = resp.serving_state() else {
            panic!("expected Degraded, got {:?}", resp.serving_state());
        };
        assert!(
            reason.contains("3 index(es) failed to open their corpus"),
            "reason must name the corpus failures; got: {reason}"
        );
        assert!(
            reason.contains("EMPTY result set"),
            "reason must state the query-time consequence; got: {reason}"
        );
        assert!(
            reason.contains("11 index(es) were skipped on a warm-boot scan timeout"),
            "reason must name the skipped indexes; got: {reason}"
        );
    }

    /// trusty-search #5927: a lane failure with a healthy corpus must still
    /// produce a reason, and must still classify as `Degraded`.
    ///
    /// Why: this is the fail-open the #5927 counter split would otherwise
    /// create here. Before the split, an index whose semantic lane died
    /// arrived on the wire as `indexes_corpus_failed: 1` and produced a
    /// clause. After it, that index arrives ONLY as `indexes_stage_failed`, so
    /// a reader that does not know the new key builds an empty clause list —
    /// and `serving_state` reads an empty list as the benign network-mount
    /// case and answers `Serving`. A real capability gap would vanish from
    /// every review that ran against that daemon, silently.
    /// What: a payload in the exact shape trusty-search now sends for that
    /// state — `indexes_stage_failed: 2` with `indexes_corpus_failed: 0` —
    /// asserting the verdict is `Degraded`, that the reason names the lane
    /// failure, and that it does NOT claim a corpus failure that did not
    /// happen.
    ///
    /// #6686 narrowed what the clause may claim: it counts indexes and says so,
    /// and it no longer asserts that the surviving lane is lexical. That claim
    /// came from subtracting two host-wide totals and was false for the index
    /// that produced #6686 — `workspace`, whose lexical lane had also failed.
    /// Test: this IS the test.
    #[test]
    fn health_response_degraded_reason_names_a_lane_failure_over_a_healthy_corpus() {
        let json = r#"{
            "status": "degraded",
            "embedder": "ready",
            "warmboot_summary": {
                "indexes_loaded": 20,
                "indexes_corpus_failed": 0,
                "indexes_stage_failed": 2,
                "warm_boot_degraded": true
            }
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        let ServingState::Degraded(reason) = resp.serving_state() else {
            panic!(
                "#5927: a failed search lane must not degrade to Serving; got {:?}",
                resp.serving_state()
            );
        };
        assert!(
            reason.contains("2 index(es) have at least one failed search lane"),
            "reason must name the lane failures; got: {reason}"
        );
        assert!(
            reason.contains("GET /indexes/{id}/status"),
            "#6686: the reason must point at the probe that can name the index and lane; \
             got: {reason}"
        );
        assert!(
            !reason.contains("LEXICAL results only"),
            "#6686: two host-wide counters cannot establish which lane survived — the reason \
             must not claim lexical results are available; got: {reason}"
        );
        assert!(
            !reason.contains("failed to open their corpus"),
            "#5927: no corpus failed here — the reason must not invent one; got: {reason}"
        );
    }

    /// trusty-search #5927 / #6686: a corpus-open failure must not be reported
    /// as a SECOND, separate cohort of broken indexes.
    ///
    /// Why: `indexes_stage_failed` is a superset of `indexes_corpus_failed`, so
    /// a reader who adds the two clauses together doubles the number of broken
    /// indexes. #5927 solved that by subtracting; #6686 removed the subtraction
    /// because the claim it justified ("the remainder return LEXICAL results
    /// only") was false. The clause now states the superset relationship in
    /// words instead, so the two numbers cannot be read as disjoint cohorts.
    /// What: the shape trusty-search sends when all three failing indexes are
    /// corpus failures — both counters read `3`. Asserts the corpus clause
    /// appears, that the lane clause reports the same 3 rather than an extra
    /// cohort, and that it says so.
    /// Test: this IS the test.
    #[test]
    fn health_response_degraded_reason_does_not_double_count_a_corpus_failure() {
        let json = r#"{
            "status": "degraded",
            "embedder": "ready",
            "warmboot_summary": {
                "indexes_loaded": 20,
                "indexes_corpus_failed": 3,
                "indexes_stage_failed": 3,
                "warm_boot_degraded": true
            }
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        let ServingState::Degraded(reason) = resp.serving_state() else {
            panic!("expected Degraded, got {:?}", resp.serving_state());
        };
        assert!(
            reason.contains("3 index(es) failed to open their corpus"),
            "reason must name the corpus failures; got: {reason}"
        );
        assert!(
            reason.contains("3 index(es) have at least one failed search lane"),
            "the lane clause must report the same 3 indexes, not a fourth cohort; got: {reason}"
        );
        assert!(
            reason.contains("a superset of any corpus failures above"),
            "#6686: the clause must say the two counts overlap, so a reader does not add \
             them; got: {reason}"
        );
    }

    // ── reachability_state (#6686) ──────────────────────────────────────────

    /// REGRESSION (#6686): `/health` answers reachability and nothing else.
    ///
    /// Why: this is the live payload from the #6686 report — one unrelated index
    /// (`workspace`) with every lane failed, on a daemon that was up,
    /// embedder-ready and answering queries for 40 other indexes. The gate used
    /// to read these counters and degrade EVERY review on the host over them.
    /// `reachability_state` must report such a daemon as plainly `Serving`; what
    /// the failed index means for a given review is `GET /indexes/{id}/status`'s
    /// question, and only for the index that review actually uses.
    /// Test: this IS the test.
    #[test]
    fn reachability_state_ignores_warm_boot_counters() {
        let json = r#"{
            "status": "degraded",
            "embedder": "ready",
            "warmboot_summary": {
                "indexes_loaded": 41,
                "indexes_corpus_failed": 1,
                "indexes_stage_failed": 1,
                "indexes_skipped_timeout": 11,
                "warm_boot_degraded": true
            }
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.reachability_state(),
            ServingState::Serving,
            "#6686: a reachable, embedder-ready daemon is SERVING — a warm-boot counter is not \
             the review gate's business"
        );
    }

    #[test]
    fn reachability_state_embedder_not_ready_is_not_serving() {
        let json = r#"{"status":"ok","embedder":"loading"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(
            matches!(resp.reachability_state(), ServingState::NotServing(_)),
            "no embedder means no semantic context at all — still a host-level outage"
        );
    }

    #[test]
    fn reachability_state_other_status_is_not_serving() {
        let json = r#"{"status":"starting","embedder":"ready"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(
            matches!(resp.reachability_state(), ServingState::NotServing(_)),
            "a daemon that says it is still starting is not serving"
        );
    }

    /// `status: "degraded"` with no `warmboot_summary` (older trusty-search, or
    /// a partial response) is degraded-with-unknown-cause — NOT an outage. The
    /// daemon answered the probe, so calling it unreachable is false; the
    /// missing detail is itself reported in the reason.
    #[test]
    fn health_response_degraded_missing_summary_is_degraded_not_fatal() {
        let json = r#"{"status":"degraded","embedder":"ready"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        let ServingState::Degraded(reason) = resp.serving_state() else {
            panic!("expected Degraded, got {:?}", resp.serving_state());
        };
        assert!(
            reason.contains("no warm-boot summary"),
            "reason must say the cause is unknown; got: {reason}"
        );
    }

    /// Any status other than "ok"/"degraded" (e.g. "starting", "error") must
    /// not serve.
    #[test]
    fn health_response_other_status_is_not_serving() {
        let json = r#"{"status":"starting","embedder":"ready"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.is_serving(), "status=starting must not serve");
    }

    /// Pins the exact wire shape `WarmBootSummary` deserialises against.
    ///
    /// Why: every field here is `#[serde(default)]`, a deliberately fail-open
    /// direction — a future trusty-search rename/removal of any of these JSON
    /// keys parses as `0`/`false` rather than erroring, which would silently
    /// erase a real warm-boot gap from the degraded reason. There is no
    /// type-level way to catch a silent rename against an unknown-fields-
    /// tolerant struct; this test is the guard instead — it hardcodes today's
    /// exact key names inside a full `warmboot_summary` object shaped like a
    /// real trusty-search response and asserts each value round-trips. If
    /// trusty-search ever renames a key, THIS SPECIFIC ASSERTION starts failing
    /// even though `serde_json::from_str` itself still reports success — that
    /// combination (parse ok, assertion fails) is the signal to go check
    /// trusty-search's actual `/health` payload for a rename before assuming
    /// this fixture merely went stale.
    /// What: deserialises a real-shaped `warmboot_summary` object and asserts
    /// every mirrored counter and the aggregate flag round-trip.
    /// Test: this test itself.
    #[test]
    fn warm_boot_summary_wire_shape_is_pinned() {
        let json = r#"{
            "status": "degraded",
            "embedder": "ready",
            "warmboot_summary": {
                "indexes_loaded": 40,
                "indexes_skipped_tcc": 2,
                "indexes_skipped_timeout": 3,
                "warm_boot_degraded": true,
                "indexes_lazy": 0,
                "indexes_failed": 1,
                "indexes_corpus_failed": 4,
                "indexes_stage_failed": 6
            }
        }"#;
        let resp: HealthResponse =
            serde_json::from_str(json).expect("must parse a real-shaped warmboot_summary object");
        let w = resp
            .warmboot_summary
            .as_ref()
            .expect("warmboot_summary must deserialise, not default to None");
        assert_eq!(
            (
                w.indexes_loaded,
                w.indexes_skipped_tcc,
                w.indexes_skipped_timeout,
                w.indexes_failed,
                w.indexes_corpus_failed,
                w.indexes_stage_failed,
                w.warm_boot_degraded,
            ),
            (40, 2, 3, 1, 4, 6, true),
            "every mirrored warmboot_summary key must round-trip; a zero/false here means \
             trusty-search renamed a key and `#[serde(default)]` swallowed it"
        );
    }
}
