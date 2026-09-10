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

/// Append one `compress` row for a run that actually shrank its input.
///
/// Why: the caller holds the two byte counts and the path label and nothing
/// else — it should not also have to know the price table, the row shape, or
/// which runs are worth a row. Taking the framework `root` as an argument (not
/// resolving it here) keeps this writer and the statusline reader on the SAME
/// root, the way [`crate::core::savings_divert::record_divert`] does.
/// What: declines on an empty `session_id`, an unpriceable session model, or a
/// non-positive byte delta; otherwise appends to
/// [`crate::core::savings::savings_log_in`]. Every failure is a log line and a
/// return — never an error to the caller.
/// Test: `a_compress_run_appends_exactly_one_row`, `passthrough_output_writes_no_row`.
pub fn record_compress(
    root: &Path,
    session_id: &str,
    bytes_before: usize,
    bytes_after: usize,
    compression_path: &str,
) {
    record_compress_with(
        root,
        session_id,
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
    bytes_before: usize,
    bytes_after: usize,
    compression_path: &str,
    price: impl FnOnce() -> Option<ParentModel>,
) {
    if session_id.trim().is_empty() {
        tracing::debug!("no session id: writing no compress savings row");
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
    if let Err(source) = append_row(&ledger, &row) {
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
        tracing::debug!(
            session_id,
            before_tokens,
            after_tokens,
            compression_path,
            "the run returned no smaller than it read; writing no savings row"
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
mod tests;
