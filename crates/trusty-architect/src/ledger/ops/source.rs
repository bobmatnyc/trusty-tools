//! An answer's provenance, checked for the Rust writer (#10780).
//!
//! Why: an Architect inference restored after a /clear as an owner ruling cut
//! the owner's helper count; the ledger now records who an answer came from.
//! An owner source is only true when its quote is in the cited owner turn,
//! and that record (the owner-turn file, the dashboard store, the palace)
//! lives with the Architect. The Architect's `datg.py` checks it through
//! `relay_provenance.py` before it writes; this writer cannot, so it never
//! writes `by: owner` (critic H3). It still reads and keeps one.
//! What: [`check`] turns a [`SourceInput`] into the stored [`AnswerSource`]:
//! `architect` needs `basis` (one line, at most 200) and nothing else;
//! `owner` is refused, whatever it carries, as `datg.py` refuses one it
//! cannot verify.
//! Test: `ledger_writer_matches_prototype_bytes`,
//! `ledger_answer_source_round_trips_and_refuses_bad_shapes`.

use crate::error::LedgerError;
use crate::ledger::limits::check_line;
use crate::ledger::model::{AnswerSource, Extra};
use crate::ledger::rpc::SourceInput;

fn bad(msg: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(msg.into())
}

/// The stored source for `s`, or a refusal.
pub(crate) fn check(s: &SourceInput) -> Result<AnswerSource, LedgerError> {
    match s.by.as_str() {
        "architect" => {
            if s.cite.is_some() || s.quote.is_some() || s.until.is_some() {
                return Err(bad("source by architect takes only basis"));
            }
            let basis = check_line("source.basis", s.basis.as_deref().unwrap_or(""), 200, true)?;
            Ok(AnswerSource {
                by: s.by.clone(),
                cite: None,
                quote: None,
                until: None,
                basis: Some(basis),
                extra: Extra::new(),
            })
        }
        // #10780 critic H3: only datg.py can check an owner quote; refuse here.
        "owner" => Err(bad(
            "source by owner is written only by the Architect's datg.py, which checks the quote",
        )),
        _ => Err(bad("source.by must be owner or architect")),
    }
}
