//! An answer's provenance, checked as the prototype's `_source` does (#10780).
//!
//! Why: an Architect inference restored after a /clear as an owner ruling cut
//! the owner's helper count; the ledger now records who an answer came from.
//! What: [`check`] turns an [`SourceInput`] into the stored [`AnswerSource`]:
//! `owner` needs `cite` (one line, at most 80) and `quote` (one line, at most
//! 300) and may carry `until` (a UTC minute or second, kept as given);
//! `architect` needs `basis` (one line, at most 200) and nothing else.
//! Test: `ledger_writer_matches_prototype_bytes`,
//! `ledger_answer_source_round_trips_and_refuses_bad_shapes`.

use crate::error::LedgerError;
use crate::ledger::limits::{check_line, normalize_ts};
use crate::ledger::model::{AnswerSource, Extra};
use crate::ledger::rpc::SourceInput;

fn bad(msg: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(msg.into())
}

/// The stored source for `s`, or the prototype's refusal.
pub(crate) fn check(s: &SourceInput) -> Result<AnswerSource, LedgerError> {
    let line = |name: &str, v: &Option<String>, max: usize| {
        check_line(name, v.as_deref().unwrap_or(""), max, true)
    };
    match s.by.as_str() {
        "architect" => {
            if s.cite.is_some() || s.quote.is_some() || s.until.is_some() {
                return Err(bad("source by architect takes only basis"));
            }
            Ok(AnswerSource {
                by: s.by.clone(),
                cite: None,
                quote: None,
                until: None,
                basis: Some(line("source.basis", &s.basis, 200)?),
                extra: Extra::new(),
            })
        }
        "owner" => {
            if s.basis.is_some() {
                return Err(bad("source by owner takes only cite, quote, until"));
            }
            let cite = Some(line("source.cite", &s.cite, 80)?);
            let quote = Some(line("source.quote", &s.quote, 300)?);
            let until = match &s.until {
                None => None,
                Some(u) => {
                    let u = check_line("source.until", u, 20, true)?;
                    normalize_ts(&u).map_err(|_| bad("source.until must be YYYY-MM-DDTHH:MMZ"))?;
                    Some(u)
                }
            };
            Ok(AnswerSource {
                by: s.by.clone(),
                cite,
                quote,
                until,
                basis: None,
                extra: Extra::new(),
            })
        }
        _ => Err(bad("source.by must be owner or architect")),
    }
}
