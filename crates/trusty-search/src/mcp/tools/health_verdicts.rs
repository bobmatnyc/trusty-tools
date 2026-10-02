//! The pure verdict helpers `search_health` branches on (#5264, #9059).
//!
//! Why: each reads one field of a daemon body and decides nothing else, so
//! they live apart from the probing and report assembly in `health.rs`,
//! which keeps that file under the 500-SLOC cap.
//! What: embedder state, failed migration stages, an unknown chunk count's
//! cause, and (#9059) the reason an index is held.
//! Test: `mcp/tools/tests_health.rs`.

use serde_json::Value;

/// Whether `/health` reports the embedder as failed or not answering (#8348).
pub(super) fn embedder_unavailable(health: &Value) -> bool {
    matches!(
        health.get("embedder").and_then(Value::as_str),
        Some("error" | "stalled")
    )
}

/// The failed migration stages the daemon reported, comma-joined (#7979).
///
/// Why: `search_health`'s verdict has to branch on whether a migration failed,
/// and the branch must not depend on the field's exact shape drifting — an
/// absent key, `null`, and an empty array all mean "no fault".
/// What: reads `migration_error` from the status body and joins each entry's
/// `stage`. Returns `None` when there is nothing outstanding.
/// Test: `search_health_reports_a_failed_migration_instead_of_prescribing_a_reindex`.
pub(super) fn failed_migration_stages(body: &Value) -> Option<String> {
    let entries = body.get("migration_error")?.as_array()?;
    let stages: Vec<&str> = entries
        .iter()
        .filter_map(|e| e.get("stage").and_then(Value::as_str))
        .collect();
    (!stages.is_empty()).then(|| stages.join(", "))
}

/// Why the daemon could not state a chunk count, and what to do about it.
///
/// Why (#5633): "0 chunks because the index is empty" and "count unavailable
/// because the corpus would not open" demand OPPOSITE actions — reindex, versus
/// do not reindex because the index is write-quarantined and its chunks are
/// intact on disk. Rendering both the same way is what sent a caller to
/// `trusty-search index` against a quarantined corpus.
/// What: reads the `corpus_open_failure` block the daemon already sends beside
/// a null count (#4333) rather than inventing a cause, and lets its `transient`
/// classifier pick between "retry" and "this needs operator action". Returns
/// `(because, remediation)`; when no failure block is present the cause is
/// genuinely unknown and the remediation says exactly that.
/// Test: `search_health_does_not_report_an_unreadable_chunk_count_as_empty`,
/// `search_health_does_not_report_a_missing_chunk_count_as_empty`.
pub(super) fn unknown_count_cause(body: &Value) -> (String, &'static str) {
    let Some(failure) = body.get("corpus_open_failure").filter(|v| !v.is_null()) else {
        return (
            "its chunk count was absent from the daemon's status response".to_string(),
            "Re-run `search_health`. If the count stays unreadable, check \
             `trusty-search status` and the daemon log. Do NOT reindex on this \
             verdict alone — nothing here says the index is empty.",
        );
    };
    let field = |k: &str| failure.get(k).and_then(Value::as_str).unwrap_or("unknown");
    let because = format!(
        "its durable corpus failed to open ({}: {}), so the daemon reported the \
         count as unknown rather than guessing at one",
        field("kind"),
        field("reason"),
    );
    let remediation = if failure
        .get("transient")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "Retry shortly — the daemon classified this corpus failure as transient \
         (typically an open timeout under warm-boot contention). Do NOT reindex: \
         the chunks are intact on disk and the index is write-quarantined until \
         the corpus opens."
    } else {
        "The daemon classified this corpus failure as NOT transient, so retrying \
         will not clear it. Restart the daemon (`trusty-search stop` then \
         `trusty-search start`) and re-check. Do NOT reindex on this verdict — \
         the count is unknown, not zero."
    };
    (because, remediation)
}

/// The reason the daemon gave for holding this index, if it is held (#9059).
///
/// Why: a held index still answers searches, so every chunk-count arm would
/// report it `ok` while it indexes nothing new.
/// What: `Some(last_walk_error)` when the status body says `status: "held"`;
/// the daemon puts the hold reason, naming the invalid glob, in that field.
/// Test: `search_health_reports_a_held_index`.
pub(super) fn held_reason(body: &Value) -> Option<String> {
    (body.get("status").and_then(Value::as_str) == Some("held")).then(|| {
        body.get("last_walk_error")
            .and_then(Value::as_str)
            .unwrap_or("an exclude glob does not parse")
            .to_string()
    })
}
