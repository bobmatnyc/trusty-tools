//! The bulk-read diversion savings producer (#6959).
//!
//! Why: `tm divert bulk-read` (#6887) already answers a blocked read on a cheap
//! Haiku worker and prints a paragraph where the session would have ingested a
//! whole file. What it did not do was say what that was worth. This module is
//! the second producer for the #6958 ledger: one row per successful diversion,
//! so the `💸` statusline segment folds real diversion traffic alongside the
//! instruction-compression rows.
//!
//! What: [`record_divert`], called from `tm divert bulk-read`'s success arm. It
//! prices the token delta — the file bytes the parent never read, minus the
//! summary bytes it did — at the PARENT session's input rate, subtracts what the
//! worker itself billed, and appends one
//! [`crate::core::savings::SavingsRow`] with `technique = "divert"`.
//!
//! **The formula, in one sentence.** `tokens_saved` is the diverted files'
//! token count minus the returned summary's; `cost_saved_usd` is that delta at
//! the parent model's published input rate, minus the worker's own reported
//! cost.
//!
//! Three properties the feature depends on:
//!
//! - **Only a successful diversion writes.** A fall-through — no worker, a
//!   worker error, `is_error` in the child's JSON — never reaches this module,
//!   because the caller exits on that path before it is called.
//! - **A non-positive delta writes nothing.** A summary at least as large as the
//!   files it summarises saved nothing, and a worker that cost more than the
//!   avoided tokens were worth saved nothing either. Both decline rather than
//!   report a fabricated positive.
//! - **A ledger failure is not a diversion failure.** The summary is already on
//!   stdout by the time this runs. An unwritable ledger logs a `warn!` and
//!   nothing else — the agent still gets its answer.
//!
//! Test: the inline suite in `savings_divert_tests.rs` —
//! `a_hand_computed_delta_matches_the_row`,
//! `no_row_when_the_summary_is_not_smaller_than_the_files`,
//! `divert_row_carries_the_named_technique`,
//! `no_row_when_the_parent_model_cannot_be_priced`,
//! `a_ledger_write_failure_does_not_fail_the_diversion`.

use std::path::Path;

use crate::core::savings::{BYTES_PER_TOKEN, SavingsRow, TECHNIQUE_DIVERT, append_row, now_ts};

/// The environment variable naming the model the PARENT session runs on.
///
/// Why (#6959): the row is attributed to the parent session, so it must be
/// priced at the parent's rate — pricing a diverted Opus read at Haiku's rate
/// would understate the saving by nearly twenty times. Claude Code exports this
/// variable when the operator pins a model through it, and `tm divert` runs as a
/// plain child of the session, so it arrives unmodified. The worker's own
/// environment scrub (`divert_worker::NESTED_SESSION_ENV`) applies to the CHILD
/// this command spawns, never to this process.
/// What: `ANTHROPIC_MODEL`. Absent or blank falls back to
/// [`resolve_session_price`]'s config chain.
/// Test: `resolve_session_price_agrees_with_the_shared_table`.
pub const SESSION_MODEL_ENV: &str = "ANTHROPIC_MODEL";

/// The smallest cost delta worth a row.
///
/// Why (#6959): a diversion whose worker billed EXACTLY what the avoided tokens
/// were worth does not compute to `0.0` — the subtraction leaves float residue
/// around `4e-17`, which a bare `> 0.0` test accepts. That row would then carry
/// its full `tokens_saved` into the statusline fold while claiming no money, so
/// a diversion that saved nothing would move the token figure. One micro-dollar
/// is the resolution the row's own `basis` prints at; below it there is no
/// measurement to report.
/// What: `1e-6` USD.
/// Test: `no_row_when_the_worker_cost_exceeds_the_saving`.
const MIN_COST_SAVED_USD: f64 = 1e-6;

/// Append one `divert` row for a diversion that answered.
///
/// Why: the caller has the three numbers and nothing else — it should not also
/// have to know the price table, the row shape, or which failures are worth a
/// row. Taking `ledger` as an argument (rather than resolving
/// `default_savings_log()` here) is what keeps the writer and the statusline
/// reader on the SAME root: `tm` resolves `--root` / `TRUSTY_MPM_ROOT` at the
/// call site, and the segment reads through the same resolver.
/// What: declines on an empty `session_id`, an unpriceable parent model, or a
/// non-positive delta; otherwise appends the row. Every failure is a `warn!` and
/// a return — never an error to the caller.
/// Test: `a_recorded_diversion_folds_into_the_session_total`,
/// `no_row_without_a_session_id`,
/// `a_ledger_write_failure_does_not_fail_the_diversion`.
pub fn record_divert(
    ledger: &Path,
    session_id: &str,
    file_bytes: usize,
    summary_bytes: usize,
    worker_cost_usd: f64,
) {
    record_divert_with(
        ledger,
        session_id,
        file_bytes,
        summary_bytes,
        worker_cost_usd,
        resolve_session_price,
    );
}

/// [`record_divert`] with the price lookup injected.
///
/// Why: the decline branches and the fail-open write branch are both assertable
/// from a tempdir this way, with no configured model and no `set_var` — this
/// crate's bin target carries an env-mutation ratchet (#5544) and a process-wide
/// mutation corrupts sibling tests regardless.
/// What: see [`record_divert`]. `price` resolves the parent model and its
/// USD-per-million input rate.
/// Test: `a_ledger_write_failure_does_not_fail_the_diversion`,
/// `no_row_without_a_session_id`.
fn record_divert_with(
    ledger: &Path,
    session_id: &str,
    file_bytes: usize,
    summary_bytes: usize,
    worker_cost_usd: f64,
    price: impl FnOnce() -> Option<(String, f64)>,
) {
    if session_id.trim().is_empty() {
        tracing::warn!("no parent session id: writing no divert savings row");
        return;
    }
    let Some(row) = divert_row(
        session_id,
        file_bytes,
        summary_bytes,
        worker_cost_usd,
        price,
    ) else {
        return;
    };
    // #6959: the answer is already on stdout — a ledger failure must not turn a
    // diversion that worked into one that failed.
    if let Err(source) = append_row(ledger, &row) {
        tracing::warn!(
            ledger = %ledger.display(),
            %source,
            "could not append the divert savings row"
        );
    }
}

/// Build the row, or decline to.
///
/// Why: separating the arithmetic from the IO is what makes every decline a unit
/// test rather than a filesystem assertion, and it is where the "never fabricate
/// a positive" rule lives.
/// What: file tokens FLOOR and summary tokens CEIL, so rounding can only
/// understate the saving. Declines when the summary is not smaller than the
/// files, when the parent model cannot be priced, or when the worker cost eats
/// the whole delta.
/// Test: `a_hand_computed_delta_matches_the_row`,
/// `no_row_when_the_summary_is_not_smaller_than_the_files`,
/// `no_row_when_the_worker_cost_exceeds_the_saving`.
fn divert_row(
    session_id: &str,
    file_bytes: usize,
    summary_bytes: usize,
    worker_cost_usd: f64,
    price: impl FnOnce() -> Option<(String, f64)>,
) -> Option<SavingsRow> {
    let file_tokens = (file_bytes as f64 / BYTES_PER_TOKEN).floor() as i64;
    let summary_tokens = (summary_bytes as f64 / BYTES_PER_TOKEN).ceil() as i64;
    let tokens_saved = file_tokens - summary_tokens;
    if tokens_saved <= 0 {
        tracing::debug!(
            session_id,
            file_tokens,
            summary_tokens,
            "the diversion returned no smaller than it read; writing no savings row"
        );
        return None;
    }
    let Some((model, input_per_million)) = price() else {
        tracing::warn!(
            session_id,
            "the parent session's model is unknown or unpriceable; writing no divert savings row"
        );
        return None;
    };
    let cost_saved_usd = (tokens_saved as f64 / 1_000_000.0) * input_per_million - worker_cost_usd;
    if !cost_saved_usd.is_finite() || cost_saved_usd < MIN_COST_SAVED_USD {
        tracing::debug!(
            session_id,
            tokens_saved,
            worker_cost_usd,
            "the worker cost the whole delta; writing no savings row"
        );
        return None;
    }
    Some(SavingsRow {
        ts: now_ts(),
        session_id: session_id.to_string(),
        technique: TECHNIQUE_DIVERT.to_string(),
        tokens_saved,
        cost_saved_usd,
        basis: format!(
            "files {file_tokens} tok - summary {summary_tokens} tok, \
             at {BYTES_PER_TOKEN} B/token, priced at {model} input \
             ${input_per_million}/Mtok, less worker ${worker_cost_usd:.6}"
        ),
    })
}

/// Resolve the PARENT session's model and its input price.
///
/// Why: the saving is what the parent did not spend, so it is priced at the
/// parent's rate, never the worker's. The price table is
/// `trusty_common::inference::pricing` — the one this feature already uses — and
/// a model it does not know declines the row rather than substituting a guess.
/// What: [`SESSION_MODEL_ENV`] when the harness exported it, otherwise the same
/// `resolve_pm_model` chain that produced the session's own `--model` flag.
/// Returns `(slug, input USD per million tokens)`.
/// Test: `resolve_session_price_agrees_with_the_shared_table`.
fn resolve_session_price() -> Option<(String, f64)> {
    let model = std::env::var(SESSION_MODEL_ENV)
        .ok()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            let config = crate::core::config::MpmConfig::load_default();
            crate::core::model_inject::resolve_pm_model(&config, None)
        });
    let pricing = trusty_common::inference::pricing(&model)?;
    Some((model, pricing.input))
}

#[cfg(test)]
#[path = "savings_divert_tests.rs"]
mod tests;
