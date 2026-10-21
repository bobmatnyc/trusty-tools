//! Field limits, the size bar, timestamps and the schema check.
//!
//! Why: the prototype enforced these in code only (#10062 gap G16); the port
//! needs one definition the writer, the daemon method and the CLI share.
//! What: the limits as constants, [`check_title`] (one line, at most 120
//! characters), [`check_paragraph`] (no blank line), [`size_verdict`]
//! (LDG-2.8.5: warn above 8 KB, refuse above 16 KB when the file grows),
//! [`normalize_ts`] (LDG-2.3.6: minute input gains `:00`) and
//! [`check_writable`] (#9757 AC4: refuse a newer schema). These check a
//! write; a store read keeps whatever it holds (live stores carry titles over
//! 120 characters, written before the limit).
//! Test: `ledger_title_and_paragraph_limits`, `ledger_size_bar_on_fixture_sizes`,
//! `ledger_timestamps_normalize_to_seconds`,
//! `ledger_writer_refuses_newer_schema_and_leaves_bytes`.

use super::model::State;
use crate::error::LedgerError;

/// The schema this crate writes; a store with a higher `schema` is refused.
pub const SCHEMA_VERSION: u64 = 1;
/// `title` limit, in characters.
pub const TITLE_MAX: usize = 120;
/// `answer` and `outcome` limit, in characters.
pub const PARAGRAPH_MAX: usize = 300;
/// `state.json` size that logs `size_warning`.
pub const SIZE_WARN: u64 = 8 * 1024;
/// `state.json` size a growing write may not exceed.
pub const SIZE_REFUSE: u64 = 16 * 1024;
/// Session record limit, in bytes of compact JSON.
pub const SESSION_MAX: usize = 2048;

/// What the size bar says about a write from `before` to `after` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeVerdict {
    /// At or under the warn bar.
    Ok,
    /// Over the warn bar; written, with a `size_warning` log line.
    Warn,
    /// Over the refuse bar and larger than before; not written.
    Refuse,
}

/// The size bar (LDG-2.8.5). A write that shrinks an oversized store is
/// allowed, so a store over the bar can always be pruned.
pub fn size_verdict(before: u64, after: u64) -> SizeVerdict {
    if after > SIZE_REFUSE && after > before {
        SizeVerdict::Refuse
    } else if after > SIZE_WARN {
        SizeVerdict::Warn
    } else {
        SizeVerdict::Ok
    }
}

/// A title, trimmed: non-empty, one line, at most [`TITLE_MAX`] characters.
///
/// # Errors
/// [`LedgerError::Invalid`] naming the rule.
pub fn check_title(title: &str) -> Result<&str, LedgerError> {
    let t = title.trim();
    if t.is_empty() || t.contains('\n') || t.chars().count() > TITLE_MAX {
        return Err(LedgerError::Invalid(format!(
            "title required, one line, <= {TITLE_MAX} chars"
        )));
    }
    Ok(t)
}

/// A one-paragraph field (`answer`, `outcome`, `detail`): at most `max`
/// characters and no blank line (LDG-2.3.4).
///
/// # Errors
/// [`LedgerError::Invalid`] naming the field.
pub fn check_paragraph<'a>(name: &str, text: &'a str, max: usize) -> Result<&'a str, LedgerError> {
    let t = text.trim();
    let blank_line = t.lines().any(|l| l.trim().is_empty());
    if blank_line || t.chars().count() > max {
        return Err(LedgerError::Invalid(format!(
            "{name} must be one paragraph, <= {max} chars"
        )));
    }
    Ok(t)
}

/// `YYYY-MM-DDTHH:MM[:SS]Z` as `YYYY-MM-DDTHH:MM:SSZ` (LDG-2.3.6).
///
/// # Errors
/// [`LedgerError::Invalid`] for any other form or an out-of-range field.
pub fn normalize_ts(ts: &str) -> Result<String, LedgerError> {
    let bad = || LedgerError::Invalid(format!("timestamp {ts:?}: want YYYY-MM-DDTHH:MM:SSZ (UTC)"));
    let body = ts.strip_suffix('Z').ok_or_else(bad)?;
    let (date, time) = body.split_once('T').ok_or_else(bad)?;
    let num = |s: &str, len: usize, lo: u32, hi: u32| {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
            .filter(|n| (lo..=hi).contains(n))
    };
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    let date_ok = matches!(d.as_slice(), [y, m, dd]
        if num(y, 4, 1970, 9999).is_some() && num(m, 2, 1, 12).is_some() && num(dd, 2, 1, 31).is_some());
    let secs = match t.as_slice() {
        [h, m] if num(h, 2, 0, 23).is_some() && num(m, 2, 0, 59).is_some() => "00",
        [h, m, s]
            if num(h, 2, 0, 23).is_some()
                && num(m, 2, 0, 59).is_some()
                && num(s, 2, 0, 59).is_some() =>
        {
            s
        }
        _ => return Err(bad()),
    };
    if !date_ok {
        return Err(bad());
    }
    Ok(format!("{date}T{}:{}:{secs}Z", t[0], t[1]))
}

/// Refuses a store this writer may not write: a newer `schema`.
///
/// # Errors
/// [`LedgerError::NewerSchema`].
pub fn check_writable(state: &State) -> Result<(), LedgerError> {
    let found = state.schema_version();
    if found > SCHEMA_VERSION {
        return Err(LedgerError::NewerSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}
