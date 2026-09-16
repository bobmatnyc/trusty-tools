//! Session-related output formatters.
//!
//! Why: session list and event display share compact-output helpers that should
//! live outside the handler file to keep it below the 500-line cap.
//! What: `short_id` for UUID truncation, `event_summary` for payload one-liners,
//! `print_compression_stats` for optimizer feedback, `deploy_summary_line` for
//! the agent/skill deploy-count summaries `session start` prints.
//! Test: `short_id_*` and `event_summary_*` unit tests in `tests.rs`; the
//! compression stats helper is exercised by `compression_stats_line_*` tests;
//! `deploy_summary_line` by `deploy_summary_line_formats_counts`.

/// Render a `SessionId` JSON value into a short, human id.
///
/// Why (#7805): `SessionId` is a plain `#[derive(Serialize)]` newtype over
/// `Uuid`, and serde writes a newtype struct TRANSPARENTLY — the daemon's wire
/// value is the bare string `"<uuid>"`, never the `{"0": "<uuid>"}` tuple shape
/// this helper used to be the only reader of. Every row therefore rendered the
/// `--------` placeholder, and `tm sessions list` (and its `tm session` alias)
/// printed `-------- Starting <path>`, which reads as a start-progress line
/// rather than an id/status/workdir row — the misreading #7805 was filed on.
/// What: reads the bare-string wire form first, then the `{"0": …}` object form
/// any older producer may still emit, and truncates the uuid to its first 8
/// characters. A value that is neither still falls back to the placeholder.
/// Test: `short_id_reads_the_daemon_string_wire_form`,
/// `short_id_extracts_uuid_prefix`, `short_id_truncates_to_eight_chars`,
/// `short_id_falls_back_when_field_missing`,
/// `short_id_falls_back_when_value_not_str`.
pub(crate) fn short_id(value: &serde_json::Value) -> String {
    // #7805: the string arm is the real daemon shape; the object arm is legacy.
    value
        .as_str()
        .or_else(|| value.get("0").and_then(|v| v.as_str()))
        .map(|s| s.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "--------".to_string())
}

/// Summarize an opaque hook-event payload into a single short line.
///
/// Why: `session events` prints one row per event; a full JSON payload would
/// wrap the terminal, so a compact summary keeps rows readable.
/// What: shows the `tool` field when present, otherwise a truncated JSON dump.
/// Test: covered by `event_summary_*` unit tests.
pub(crate) fn event_summary(payload: &serde_json::Value) -> String {
    if let Some(tool) = payload.get("tool").and_then(|v| v.as_str()) {
        return format!("tool={tool}");
    }
    let dump = payload.to_string();
    if dump.len() > 60 {
        format!("{}…", &dump[..60])
    } else {
        dump
    }
}

/// Print a one-line compression-savings note when an output was summarized.
///
/// Why: `session run --summarize` and `session output --summarize` should tell
/// the operator how much the summary saved, completing the visible feedback for
/// the "summarize output" step of the user cycle.
/// What: when the response body carries a non-null `compress_level`, prints
/// `[summarized: A → B bytes (N% reduction)]` to a fresh line; does nothing when
/// the output was returned raw (no compression applied).
/// Test: `compression_stats_line_*` unit tests.
pub(crate) fn print_compression_stats(body: &serde_json::Value) {
    if body
        .get("compress_level")
        .and_then(|v| v.as_str())
        .is_none()
    {
        return;
    }
    let original = body
        .get("original_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let compressed = body
        .get("compressed_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let reduction = (100 * original.saturating_sub(compressed))
        .checked_div(original)
        .unwrap_or(0);
    println!("\n[summarized: {original} \u{2192} {compressed} bytes ({reduction}% reduction)]");
}

/// Render a "`<Label>: N deployed, M skipped, K unchanged`" summary line.
///
/// Why (#1917): `session start` prints matching three-way deploy summaries
/// for both agents and skills; a shared formatter keeps the two counts in
/// lockstep instead of two near-identical `println!` calls drifting apart —
/// before this fix, only the agent line existed, and `report.skill_deploy`
/// was silently never surfaced anywhere.
/// What: formats `"{label}: {deployed} deployed, {skipped} skipped, {unchanged} unchanged"`.
/// Test: `deploy_summary_line_formats_counts`.
pub(crate) fn deploy_summary_line(
    label: &str,
    deployed: usize,
    skipped: usize,
    unchanged: usize,
) -> String {
    format!("{label}: {deployed} deployed, {skipped} skipped, {unchanged} unchanged")
}

/// Render the `session start` delegation-roster line, declaring the count
/// incomplete when a roster read failed.
///
/// Why (#5544): this line is where `tm session start` publishes
/// [`trusty_mpm::core::instruction_pipeline::PipelineOutput::agent_count`], and
/// an unreadable agent file lowers that number with nothing to distinguish it
/// from a genuinely smaller roster. Printing the bare count was the same
/// incomplete-result-reported-as-complete shape the composed prompt's
/// `ROSTER INCOMPLETE` banner exists to remove, surviving one layer up.
/// What: the plain `"Instructions: N agents in delegation authority"` when
/// nothing was lost; otherwise `N` is qualified as `at least N` and the line
/// carries `ROSTER INCOMPLETE` with the unreadable paths named.
/// Test: `session_start_roster_line_is_plain_when_the_roster_is_whole`,
/// `session_start_roster_line_declares_an_incomplete_roster`.
pub(crate) fn delegation_roster_line(
    agent_count: usize,
    unreadable: &[std::path::PathBuf],
) -> String {
    if unreadable.is_empty() {
        return format!("Instructions: {agent_count} agents in delegation authority");
    }
    let paths: Vec<String> = unreadable.iter().map(|p| p.display().to_string()).collect();
    format!(
        "Instructions: at least {agent_count} agents in delegation authority — \
         ROSTER INCOMPLETE: {} path(s) could not be read, so the real count is higher ({})",
        unreadable.len(),
        paths.join(", "),
    )
}
