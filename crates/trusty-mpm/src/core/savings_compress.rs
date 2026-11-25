//! The tool-output compression savings producer.
//!
//! Why: `tm compress` is the third technique the harness spends effort on — it
//! shrinks a gate's or a bash command's output before an agent ever reads it.
//! Until this module existed it wrote only its own `compression.jsonl`
//! telemetry, so the `💸` statusline segment reported instruction folding and
//! bulk-read diversion but never bash output. The owner asked whether bash
//! compression was included; it is, from here on.
//!
//! What: [`record_compress`], called from `tm compress`'s success arm beside
//! the durable `CompressionRecord` append. It prices the byte delta the agent
//! never read at the session's own input rate and appends one
//! [`crate::core::savings::SavingsRow`] with `technique = "compress"`. The
//! row's `basis` carries the `compression_path` the run took (`rtk_binary` or
//! `native_fallback`) so a later attribution pass can split the two.
//!
//! Three properties it shares with the divert producer, deliberately:
//!
//! - **A non-positive delta writes nothing.** `tm compress` passes short output
//!   through unchanged, and that run avoided nothing. A zero-saving row would
//!   still carry a `tokens_before` into the fold and drag the average down, so
//!   the run declines instead.
//! - **The price comes from the session, not a default.**
//!   [`crate::core::savings_divert::resolve_session_price`] is the one resolver
//!   both producers call, so a compress row and a divert row in the same session
//!   can never be priced at two different models.
//! - **A ledger failure is not a compression failure.** The compressed text is
//!   the caller's return value; an unwritable ledger logs a `warn!` and nothing
//!   else.
//!
//! Test: the inline suite in `savings_compress_tests.rs` —
//! `a_compress_run_appends_exactly_one_row`,
//! `a_compress_row_carries_the_named_technique`,
//! `a_hand_computed_byte_delta_matches_the_row`,
//! `passthrough_output_writes_no_row`,
//! `an_expanded_output_writes_no_row`,
//! `the_basis_names_the_compression_path`,
//! `no_row_without_a_session_id`,
//! `no_row_when_the_session_model_cannot_be_priced`,
//! `a_ledger_write_failure_does_not_fail_the_compression`.

use std::path::Path;

use crate::core::savings::{BYTES_PER_TOKEN, SavingsRow, TECHNIQUE_COMPRESS, append_row, now_ts};
use crate::core::savings_divert::{MIN_COST_SAVED_USD, ParentModel, resolve_session_price};
use crate::core::savings_opportunity::{
    OPPORTUNITY_DAY_CAP_BYTES, OPPORTUNITY_KEEP_DAYS, TOOL_TYPE_BASH, TaggedSavingsRow,
    append_day_line,
};

/// Append one `compress` row for a run that actually shrank its input.
///
/// Why: the caller holds the two byte counts and the path label and nothing
/// else — it should not also have to know the price table, the row shape, or
/// which runs are worth a row. Taking the framework `root` as an argument (not
/// resolving it here) keeps this writer and the statusline reader on the SAME
/// root, the way [`crate::core::savings_divert::record_divert`] does.
/// What: declines on an empty `session_id`, an unpriceable session model, or a
/// non-positive byte delta; otherwise appends — tagged `tool_type: bash` and
/// `tool_name` (#10973) — to
/// [`crate::core::savings::savings_log_in`]. Every failure is a log line and a
/// return — never an error to the caller.
/// Test: `a_compress_run_appends_exactly_one_row`, `passthrough_output_writes_no_row`.
pub fn record_compress(
    root: &Path,
    session_id: &str,
    tool_name: &str,
    bytes_before: usize,
    bytes_after: usize,
    compression_path: &str,
) {
    record_compress_with(
        root,
        session_id,
        tool_name,
        bytes_before,
        bytes_after,
        compression_path,
        || resolve_session_price(root, session_id),
    );
}

/// [`record_compress`] with the price lookup injected.
///
/// Why: the decline branches and the fail-open write branch are assertable from
/// a tempdir this way, with no configured model and no process-wide env
/// mutation (#5544).
/// What: see [`record_compress`]. `price` resolves the session's model, its
/// USD-per-million input rate, and which source named it.
/// Test: `no_row_without_a_session_id`,
/// `a_ledger_write_failure_does_not_fail_the_compression`.
fn record_compress_with(
    root: &Path,
    session_id: &str,
    tool_name: &str,
    bytes_before: usize,
    bytes_after: usize,
    compression_path: &str,
    price: impl FnOnce() -> Option<ParentModel>,
) {
    record_tagged_with(
        root,
        session_id,
        (tool_name, None),
        (bytes_before, bytes_after),
        compression_path,
        price,
    );
}

/// One step of a chained Bash call, for the row that step writes (#11507).
#[derive(Debug, Clone, Copy)]
pub struct ChainTag<'a> {
    /// The chain's sanitised shape, e.g. `cd ; git diff && cargo test`.
    pub shape: &'a str,
    /// How many steps the chain has.
    pub steps: usize,
}

/// [`record_compress`] for one compressed step of a chain (#11507).
///
/// What: the same decline rules and row, with `tool_name` the step's own tool
/// and the additive `chain_shape`/`steps` fields set from `chain`.
/// Test: `a_chain_step_row_carries_its_tool_and_the_chain_fields`.
pub fn record_compress_step(
    root: &Path,
    session_id: &str,
    tool_name: &str,
    (bytes_before, bytes_after): (usize, usize),
    compression_path: &str,
    chain: ChainTag<'_>,
) {
    record_tagged_with(
        root,
        session_id,
        (tool_name, Some(chain)),
        (bytes_before, bytes_after),
        compression_path,
        || resolve_session_price(root, session_id),
    );
}

/// [`record_compress_with`] carrying the #11507 chain tag, when there is one.
///
/// Test: `a_chain_step_row_carries_its_tool_and_the_chain_fields`.
pub(crate) fn record_tagged_with(
    root: &Path,
    session_id: &str,
    (tool_name, chain): (&str, Option<ChainTag<'_>>),
    (bytes_before, bytes_after): (usize, usize),
    compression_path: &str,
    price: impl FnOnce() -> Option<ParentModel>,
) {
    if session_id.trim().is_empty() {
        tracing::debug!("no session id: writing no compress savings row");
        return;
    }
    // #11563: both ledgers store the sanitised name, never a path or pattern.
    let tool_name = &crate::core::savings_opportunity::ledger_tool_name(tool_name);
    if !compress_gains(bytes_before, bytes_after) {
        // #11560: a zero-gain decline goes to a file, never to stderr.
        record_decline(
            root,
            session_id,
            tool_name,
            (bytes_before, bytes_after),
            compression_path,
        );
        return;
    }
    let Some(row) = compress_row(
        session_id,
        bytes_before,
        bytes_after,
        compression_path,
        price,
    ) else {
        return;
    };
    let ledger = crate::core::savings::savings_log_in(root);
    // The compressed text is already the caller's return value — a ledger
    // failure must not turn a compression that worked into one that failed.
    // #10973: tagged with the tool, so savings break down per tool type.
    let tagged = TaggedSavingsRow {
        row: &row,
        tool_type: TOOL_TYPE_BASH,
        tool_name,
        chain_shape: chain.map(|c| c.shape),
        steps: chain.map(|c| c.steps),
    };
    if let Err(source) = append_row(&ledger, &tagged) {
        tracing::warn!(
            ledger = %ledger.display(),
            %source,
            "could not append the compress savings row"
        );
    }
}

/// Build the row, or decline to.
///
/// Why: separating the arithmetic from the IO makes every decline a unit test
/// rather than a filesystem assertion, and it is where the "never fabricate a
/// positive" rule lives for this technique.
/// What: before-tokens FLOOR and after-tokens CEIL, so rounding can only
/// understate the saving — the same divisor and the same direction the divert
/// producer uses, so the two techniques' figures stay comparable. Declines when
/// the compressed output is not smaller than the input (the passthrough case),
/// when the session's model cannot be priced, or when the priced delta rounds
/// below [`MIN_COST_SAVED_USD`].
/// Test: `a_hand_computed_byte_delta_matches_the_row`,
/// `passthrough_output_writes_no_row`, `an_expanded_output_writes_no_row`,
/// `no_row_when_the_session_model_cannot_be_priced`.
/// The directory of day files a zero-gain compress decline is appended to (#11560).
///
/// What: `<root>/usage/compress-declines/<YYYY-MM-DD>.jsonl`, bounded like the
/// opportunity ledger.
pub fn decline_dir_in(root: &Path) -> std::path::PathBuf {
    root.join("usage").join("compress-declines")
}

/// Append one line naming why a run wrote no savings row (#11560).
///
/// Why: the decline was a `debug!` that the `info` filter hid, and stderr is
/// the agent's tool result, so #7607 forbids printing it there. A file under
/// the usage dir is where an operator can count it; `compression.jsonl`
/// already counts the run itself.
/// What: one JSON line `{ts, session_id, tool_name, reason: "no_gain",
/// bytes_before, bytes_after, compression_path}` to today's file under
/// [`decline_dir_in`].
/// FAIL-OPEN and silent: a failed write is a `debug!`, never stderr at `info`.
/// Test: `a_zero_saving_decline_is_recorded_in_the_decline_file`,
/// `a_failed_decline_write_is_silent`.
fn record_decline(
    root: &Path,
    session_id: &str,
    tool_name: &str,
    (bytes_before, bytes_after): (usize, usize),
    compression_path: &str,
) {
    let ts = now_ts();
    let line = serde_json::json!({
        "ts": ts,
        "session_id": session_id,
        "tool_name": tool_name,
        "reason": "no_gain",
        "bytes_before": bytes_before,
        "bytes_after": bytes_after,
        "compression_path": compression_path,
    });
    // #11560 review: day files under the opportunity ledger's own cap; a full
    // day drops (and counts) the line, with nothing on stderr.
    let dir = decline_dir_in(root);
    if let Err(source) = append_day_line(
        &dir,
        &ts,
        &line,
        OPPORTUNITY_DAY_CAP_BYTES,
        OPPORTUNITY_KEEP_DAYS,
    ) {
        tracing::debug!(dir = %dir.display(), %source, "could not record the compress decline");
    }
}

/// Whether a run from `bytes_before` to `bytes_after` saved any tokens.
///
/// Why (#11560): the opportunity row's `compressed` / `compressed_no_gain`
/// split must use the same rounding [`compress_row`] declines on.
/// Test: `compress_gains_matches_the_row_decline`.
pub fn compress_gains(bytes_before: usize, bytes_after: usize) -> bool {
    let before_tokens = (bytes_before as f64 / BYTES_PER_TOKEN).floor() as i64;
    let after_tokens = (bytes_after as f64 / BYTES_PER_TOKEN).ceil() as i64;
    before_tokens > after_tokens
}

fn compress_row(
    session_id: &str,
    bytes_before: usize,
    bytes_after: usize,
    compression_path: &str,
    price: impl FnOnce() -> Option<ParentModel>,
) -> Option<SavingsRow> {
    let before_tokens = (bytes_before as f64 / BYTES_PER_TOKEN).floor() as i64;
    let after_tokens = (bytes_after as f64 / BYTES_PER_TOKEN).ceil() as i64;
    let tokens_saved = before_tokens - after_tokens;
    if tokens_saved <= 0 {
        // #11560: debug only — `tm compress` stderr is the agent's tool result
        // (#7607). The operator-visible record is `record_decline`'s file.
        tracing::debug!(
            session_id,
            before_tokens,
            after_tokens,
            compression_path,
            reason = "no_gain",
            "compress declined a savings row: the run returned no fewer tokens than it read"
        );
        return None;
    }
    let Some(ParentModel {
        id: model,
        input_per_million,
        source,
    }) = price()
    else {
        tracing::warn!(
            session_id,
            "the session's model is unknown or unpriceable; writing no compress savings row"
        );
        return None;
    };
    let cost_saved_usd = (tokens_saved as f64 / 1_000_000.0) * input_per_million;
    if !cost_saved_usd.is_finite() || cost_saved_usd < MIN_COST_SAVED_USD {
        tracing::debug!(
            session_id,
            tokens_saved,
            "the priced delta rounds to nothing; writing no savings row"
        );
        return None;
    }
    Some(SavingsRow {
        ts: now_ts(),
        session_id: session_id.to_string(),
        technique: TECHNIQUE_COMPRESS.to_string(),
        tokens_saved,
        // #7179: the percent segment's denominator — what the agent would have
        // read had the output not been compressed. A floor of a non-negative
        // byte count, so never negative.
        tokens_before: before_tokens as u64,
        cost_saved_usd,
        basis: format!(
            "output {before_tokens} tok - compressed {after_tokens} tok, \
             at {BYTES_PER_TOKEN} B/token, via {compression_path}, priced at \
             {model} ({source}) input ${input_per_million}/Mtok"
        ),
        model_source: source.to_string(),
    })
}

#[cfg(test)]
#[path = "savings_compress_tests.rs"]
pub(crate) mod tests;
