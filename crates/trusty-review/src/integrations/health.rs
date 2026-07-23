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
    /// Warm-boot health summary (issue #3693).
    ///
    /// Why: trusty-search downgrades its top-level `status` to `"degraded"`
    /// for two very different reasons: (1) a benign, intentional capability
    /// gap — the file watcher was auto-disabled on a network-mounted
    /// (EFS/NFS) root, indexes stay fully queryable/reindexable via the
    /// explicit `index-file`/`remove-file` API — or (2) a real fault —
    /// TCC/FDA denial, a scan timeout, a corpus that failed to open, or a
    /// large fraction of indexes missing after warm-boot. Only trusty-search
    /// itself can tell these apart; it already does, via
    /// `warmboot_summary.warm_boot_degraded`, which is `true` only for
    /// reason (2). Absent on older trusty-search versions or transport
    /// errors, in which case callers must treat degraded as "unknown, assume
    /// not serving" (see `is_serving`'s doc comment).
    /// Test: `health_response_degraded_watcher_only_is_serving`,
    /// `health_response_degraded_warmboot_degraded_is_not_serving`,
    /// `health_response_degraded_missing_summary_is_not_serving`.
    #[serde(default)]
    pub warmboot_summary: Option<WarmBootSummary>,
}

/// Minimal mirror of trusty-search's warm-boot health summary (issue #3693).
///
/// Why: trusty-review only needs the single aggregate `warm_boot_degraded`
/// flag to distinguish a benign "degraded" (network-mount watcher disable)
/// from a genuine fault (TCC denial, scan timeout, corpus-open failure, or a
/// large fraction of indexes missing) — see trusty-search's
/// `WarmBootSummary::warm_boot_degraded` doc for the full list of what folds
/// into it. Deliberately does not mirror the other counters: this consumer
/// only ever branches on the one boolean, and unknown fields are discarded
/// by serde (no `deny_unknown_fields`) so future additions on the
/// trusty-search side never break parsing here.
/// What: `warm_boot_degraded == true` means trusty-search itself considers
/// its warm-boot state broken, regardless of the top-level `status` string.
/// Test: see `HealthResponse` test list above.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WarmBootSummary {
    /// `true` when trusty-search's own warm-boot health check considers
    /// itself degraded for a real reason (not merely a network-mount
    /// watcher disable).
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

    /// Returns `true` when trusty-search is actually serving requests and can
    /// supply real code context — the signal `context_gate` should use
    /// instead of string-matching `status == "ok"` (issue #3693).
    ///
    /// Why: trusty-search 0.38.1 intentionally reports `status: "degraded"`
    /// on EFS/NFS-mounted repos purely because it auto-disabled its file
    /// watcher (an OS-level limitation, not a fault) — search itself stays
    /// 100% functional. Treating every non-`"ok"` status as "unreachable"
    /// (the pre-fix behaviour) fail-closed the gate and skipped every review
    /// on such deployments. This method inspects the structured
    /// `warmboot_summary.warm_boot_degraded` flag — trusty-search's own
    /// "am I actually broken" signal — rather than guessing from the status
    /// string, so a genuinely broken search (embedder dead, indexes
    /// unloadable, TCC-denied, scan-timed-out) still reports `false`.
    /// What: `false` if the embedder is not ready. Otherwise: `status ==
    /// "ok"` → `true`; `status == "degraded"` → `true` only when
    /// `warmboot_summary` is present AND `warm_boot_degraded == false`
    /// (missing summary — an older trusty-search or a transport-level
    /// partial response — is treated conservatively as NOT serving, since
    /// there is no way to confirm the degradation is benign); any other
    /// status (`"starting"`, `"error"`, …) → `false`.
    /// Test: `health_response_degraded_watcher_only_is_serving`,
    /// `health_response_degraded_warmboot_degraded_is_not_serving`,
    /// `health_response_degraded_missing_summary_is_not_serving`,
    /// `health_response_ok_is_serving`,
    /// `health_response_ok_embedder_not_ready_is_not_serving`,
    /// `health_response_other_status_is_not_serving`.
    pub fn is_serving(&self) -> bool {
        if !self.embedder.is_ready() {
            return false;
        }
        match self.status.as_str() {
            "ok" => true,
            "degraded" => self
                .warmboot_summary
                .as_ref()
                .map(|w| !w.warm_boot_degraded)
                .unwrap_or(false),
            _ => false,
        }
    }
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
        assert!(
            resp.is_serving(),
            "degraded solely due to watcher-network-degraded must still serve (#3693)"
        );
    }

    /// A genuinely broken warm-boot (TCC denial / scan timeout / corpus-open
    /// failure / mass index loss) must still fail-closed.
    #[test]
    fn health_response_degraded_warmboot_degraded_is_not_serving() {
        let json = r#"{
            "status": "degraded",
            "embedder": "ready",
            "warmboot_summary": {"warm_boot_degraded": true}
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(
            !resp.is_serving(),
            "warm_boot_degraded=true must not serve regardless of embedder state"
        );
    }

    /// `status: "degraded"` with no `warmboot_summary` at all (older
    /// trusty-search, or a partial/transport-shaped response) is treated
    /// conservatively as NOT serving — there is no structured signal to
    /// confirm the degradation is benign, so guessing "serving" would risk
    /// silently reviewing against a genuinely broken daemon.
    #[test]
    fn health_response_degraded_missing_summary_is_not_serving() {
        let json = r#"{"status":"degraded","embedder":"ready"}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(
            !resp.is_serving(),
            "degraded status without a warmboot_summary must not serve (conservative default)"
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
}
