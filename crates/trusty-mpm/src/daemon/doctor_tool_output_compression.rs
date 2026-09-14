//! `tm doctor` tool-output-compression probe (issue #7867).
//!
//! Why: the owner's 2026-09-14 ruling makes the `💸` segment a report on what
//! rtk- and shunt-style tool-output compression saves, per tool call. That
//! segment renders one percent and names no technique, so when it reads low an
//! operator has nowhere to ask whether ANY tool call has been compressed on
//! this machine — and the check that used to be next to it answered about the
//! instruction fold, a different measurement that shares the word
//! "compression". This row answers the segment's own question with the
//! segment's own inputs.
//!
//! What: [`check_tool_output_compression`] folds the savings ledger for
//! [`crate::core::savings::PER_CALL_TECHNIQUES`] — `compress` and `divert` —
//! and reports the row count, the tokens and estimated bytes saved, and when
//! the last row landed. `Ok` with at least one row, `Warn` with none, and
//! `Warn` naming the IO error when the ledger exists and cannot be read: an
//! unreadable ledger is not an empty one, and reporting it as zero rows would
//! state a measurement nobody took. Never `Fail` — nothing here is broken when
//! no tool call has been compressed yet. Read-only.
//! Test: the `tests` module below.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::savings::{BYTES_PER_TOKEN, PerCallFold, savings_log_in, try_fold_per_call};

/// The check's name, asserted on by the tests and by `tm doctor --json`.
const NAME: &str = "tool_output_compression";

/// Probe what tool-output compression has saved, from the `💸` segment's own
/// ledger.
///
/// Why: see the module doc.
/// What: folds `<framework_root>/usage/savings.jsonl` for the per-call
/// techniques and hands the reading to [`build_check`].
/// Test: the fold's branches are covered through `build_check`; this wrapper
/// only binds the real ledger path to it —
/// `an_unreadable_ledger_warns_with_the_error`,
/// `a_ledger_with_no_per_call_rows_warns`,
/// `a_compress_row_reports_rows_savings_and_the_timestamp`.
pub(super) fn check_tool_output_compression(framework_root: &Path) -> DoctorCheck {
    build_check(try_fold_per_call(&savings_log_in(framework_root)))
}

/// Fold the reading into a verdict (pure).
///
/// Why: every branch is then assertable without a ledger on disk, and the
/// error arm — the one that must never read as a zero — is reachable in a test
/// without making a file unreadable.
/// What: `Warn` naming the error text on `Err`; `Warn` naming the empty state
/// on a fold with no rows; `Ok` naming rows, tokens, estimated bytes and the
/// last row's timestamp otherwise. Bytes are the token count times
/// [`BYTES_PER_TOKEN`] and are labelled an estimate, because that divisor is
/// how every producer turned its byte delta into tokens in the first place.
/// Test: `an_unreadable_ledger_warns_with_the_error`,
/// `a_ledger_with_no_per_call_rows_warns`,
/// `a_compress_row_reports_rows_savings_and_the_timestamp`.
fn build_check(fold: Result<PerCallFold, String>) -> DoctorCheck {
    let fold = match fold {
        Ok(fold) => fold,
        Err(source) => {
            return DoctorCheck::new(
                NAME,
                CheckStatus::Warn,
                format!(
                    "the savings ledger could not be read, so what tool-output \
                     compression has saved is unknown: {source}"
                ),
            );
        }
    };
    if fold.total.rows == 0 {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            "no tool-output compression recorded yet for this project — no \
             `tm compress` run and no `tm divert` diversion has written a savings \
             row, so the 💸 segment has nothing of its own to show",
        );
    }
    let tokens = fold.total.tokens_saved;
    let bytes = (tokens as f64 * BYTES_PER_TOKEN) as u64;
    let last = fold.last_ts.as_deref().unwrap_or("unknown");
    DoctorCheck::new(
        NAME,
        CheckStatus::Ok,
        format!(
            "tool-output compression ACTIVE: {rows} compress/divert rows, \
             {tokens} tokens (~{bytes} B) saved, last row {last}",
            rows = fold.total.rows
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::savings::{
        SavingsRow, TECHNIQUE_COMPRESS, TECHNIQUE_INSTRUCTION_COMPRESSION, append_row,
    };

    fn row(technique: &str, ts: &str) -> SavingsRow {
        SavingsRow {
            ts: ts.to_string(),
            session_id: "sess-a".to_string(),
            technique: technique.to_string(),
            tokens_saved: 1_200,
            tokens_before: 6_000,
            cost_saved_usd: 0.02,
            basis: "fixture".to_string(),
            model_source: "statusline".to_string(),
        }
    }

    /// Fail-Open Check: an unreadable ledger must reach the operator as the
    /// error it is, never as the same zero an empty ledger produces.
    #[test]
    fn an_unreadable_ledger_warns_with_the_error() {
        let check = build_check(Err("permission denied (os error 13)".to_string()));

        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains("permission denied"),
            "the finding must carry the error text: {}",
            check.message
        );
        assert!(
            !check
                .message
                .contains("no tool-output compression recorded"),
            "an unreadable ledger is not an empty one: {}",
            check.message
        );
    }

    /// #7867: an instruction-fold row is not a tool-output compression row, so
    /// a ledger holding only those reads as nothing recorded — and the finding
    /// never repeats the instruction-fold check's text.
    #[test]
    fn a_ledger_with_no_per_call_rows_warns() {
        let dir = tempfile::tempdir().expect("temp root");
        let ledger = savings_log_in(dir.path());
        append_row(
            &ledger,
            &row(TECHNIQUE_INSTRUCTION_COMPRESSION, "2026-09-14T10:00:00Z"),
        )
        .expect("append");

        let check = check_tool_output_compression(dir.path());

        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check
                .message
                .contains("no tool-output compression recorded yet for this project"),
            "{}",
            check.message
        );
        for banned in ["instruction fold", "compiled prompt", "INACTIVE"] {
            assert!(
                !check.message.contains(banned),
                "this row must not carry the instruction-fold finding's text \
                 ({banned:?}): {}",
                check.message
            );
        }
    }

    /// The Ok arm reports all three facts the operator needs: how many rows,
    /// how much, and how recently.
    #[test]
    fn a_compress_row_reports_rows_savings_and_the_timestamp() {
        let dir = tempfile::tempdir().expect("temp root");
        let ledger = savings_log_in(dir.path());
        append_row(&ledger, &row(TECHNIQUE_COMPRESS, "2026-09-14T10:00:00Z")).expect("append");
        append_row(&ledger, &row(TECHNIQUE_COMPRESS, "2026-09-14T11:30:00Z")).expect("append");

        let check = check_tool_output_compression(dir.path());

        assert_eq!(check.status, CheckStatus::Ok);
        assert!(
            check.message.contains("2 compress/divert rows"),
            "{}",
            check.message
        );
        assert!(check.message.contains("2400 tokens"), "{}", check.message);
        assert!(check.message.contains("~9600 B"), "{}", check.message);
        assert!(
            check.message.contains("2026-09-14T11:30:00Z"),
            "the finding must carry the LAST row's timestamp: {}",
            check.message
        );
    }

    /// An absent ledger is the ordinary state on a fresh install, and reads the
    /// same as an empty one rather than as an error.
    #[test]
    fn a_missing_ledger_warns_rather_than_erroring() {
        let dir = tempfile::tempdir().expect("temp root");
        let check = check_tool_output_compression(dir.path());

        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check
                .message
                .contains("no tool-output compression recorded yet"),
            "{}",
            check.message
        );
    }
}
