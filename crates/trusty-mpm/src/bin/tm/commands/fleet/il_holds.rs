//! The holds recorded in an Architect's `local/state.md` (#10579).
//!
//! Why: a lost hold silently releases what the owner held, and a lifted or
//! historical one carried by mistake blocks work. The whole file is read, each
//! entry classified, and anything unclear stops the migration.
//! What: [`holds_in_state_md`] splits the file into entries (a line and the
//! indented lines under it) and classifies every entry whose head starts with
//! a hold format (`HOLD` as its first word, `usage state: HOLD`, `On hold`, any
//! case; #11480: an entry that only uses the word "hold" or "merge" is neither
//! a hold nor an error): lifted or released, a recorded builder band, or an
//! answered or closed item (`D03 (answered ...)`, `QUESTION 17 CLOSED`) is
//! history and not carried; `usage state: HOLD ...` and `hold: ...` (any case,
//! `**HOLD**:` too) are the active usage hold; any other entry that names a
//! merge is an active merge hold (all of them are carried, joined by " || ");
//! anything else is an `Err` naming its line. The two active kinds stay
//! separate.
//! Test: `the_state_md_fixture_of_record_classifies_every_hold`,
//! `hold_lines_are_read_from_state_md`, `ambiguous_hold_text_fails_before_the_ledger_is_created`.

/// The longest text one hold field takes.
pub(crate) const HOLD_MAX: usize = 1000;

/// The active holds found, each with its context lines joined by " | ".
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Holds {
    /// The usage hold (`usage state: HOLD ...`, `HOLD: ...`).
    pub(crate) usage: Option<String>,
    /// The merge hold (`On hold ...: no merge ...`).
    pub(crate) merge: Option<String>,
}

impl Holds {
    /// Whether any active hold was found.
    pub(crate) fn any(&self) -> bool {
        self.usage.is_some() || self.merge.is_some()
    }
}

/// `(first line number, the entry's lines)` for each entry of `text`.
fn entries(text: &str) -> Vec<(usize, Vec<&str>)> {
    let mut out: Vec<(usize, Vec<&str>)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let continuation = line.starts_with([' ', '\t']) && !line.trim().is_empty();
        match out.last_mut() {
            Some((_, lines)) if continuation => lines.push(line),
            _ => out.push((i + 1, vec![line])),
        }
    }
    out
}

/// Whether the entry's head starts with a hold format: `HOLD` / `hold` as its
/// first word, `usage state: HOLD`, or `On hold` (#11480).
fn starts_with_hold_format(h: &str) -> bool {
    let first = |s: &str| {
        s.split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("hold"))
    };
    let low = h.to_ascii_lowercase();
    first(h)
        || low
            .strip_prefix("usage state:")
            .is_some_and(|r| first(r.trim_start()))
        || low.strip_prefix("on ").is_some_and(first)
}

/// The entry's head with its bullet and emphasis stripped.
fn head(line: &str) -> &str {
    line.trim_start_matches([' ', '-', '*', '\t'])
}

/// Whether the entry is history: lifted, released, a builder band, or an
/// answered or closed item.
fn historical(first: &str, all: &str) -> bool {
    let low = all.to_ascii_lowercase();
    let h = head(first);
    let item = h.split([' ', '(', ':']).next().is_some_and(|w| {
        w == "QUESTION"
            || (w.len() >= 2
                && w.starts_with(['A', 'B', 'D', 'G', 'T', 'Q'])
                && w[1..].chars().all(|c| c.is_ascii_digit()))
    });
    low.contains("lifted")
        || low.contains("released")
        || h.to_ascii_lowercase().starts_with("builder band:")
        || (item && (low.contains("answered") || low.contains("closed") || low.contains("retired")))
}

/// The entry's text as one hold value: the head after its key, then each
/// continuation line, joined by " | ".
fn value(first_value: &str, rest: &[&str]) -> String {
    let mut parts = vec![first_value.trim().to_owned()];
    parts.extend(rest.iter().map(|l| head(l).trim().to_owned()));
    parts.join(" | ") // #11498: never shortened
}

/// The session record's byte limit (`SESSION_MAX` in `datg_store.py`).
pub(crate) const SESSION_MAX: usize = 8192; // #11498: long holds fit

/// Whether the holds (and the task/next written beside them) fit the ledger
/// session record `ses` (`Null` for a ledger not created yet). `Err` names the
/// field, its size and the limit (#11498).
/// Why: `hold_step` runs after the ledger and hooks exist, so a refused write
/// there leaves a live ledger without its hold; this check runs first.
/// What: each hold over [`HOLD_MAX`] characters is refused; then the record
/// as `datg.py session set` would write it (compact JSON plus `written_at` and
/// `written_epoch`) must stay within [`SESSION_MAX`] bytes. A field the ledger
/// already holds is not written, so it is not counted twice.
/// Test: `a_hold_that_cannot_be_recorded_stops_init_before_the_ledger`,
/// `a_1000_character_multibyte_hold_is_never_shortened`.
pub(crate) fn hold_room(
    ses: &serde_json::Value,
    holds: &Holds,
    work: &(Option<String>, Option<String>),
) -> Result<(), String> {
    let empty = |key: &str| ses[key].as_str().is_none_or(str::is_empty);
    let open = |key: &str| {
        ses[key]
            .as_str()
            .is_none_or(|v| v.trim().is_empty() || v.starts_with("needs owner input:"))
    };
    let mut probe = ses.as_object().cloned().unwrap_or_default();
    let mut written = Vec::new();
    for (field, v) in [("hold", &holds.usage), ("merge_hold", &holds.merge)] {
        let Some(v) = v.as_deref().filter(|_| empty(field)) else {
            continue;
        };
        let chars = v.chars().count();
        if chars > HOLD_MAX {
            return Err(format!(
                "the {field} from local/state.md is {chars} characters and the Intent Ledger \
                 `{field}` field holds at most {HOLD_MAX}"
            ));
        }
        written.push((field, v.len()));
        probe.insert(field.to_owned(), v.into());
    }
    if written.is_empty() {
        return Ok(());
    }
    for (field, v) in [("task", &work.0), ("next", &work.1)] {
        if let Some(v) = v.as_deref().filter(|_| open(field)) {
            probe.insert(field.to_owned(), v.into());
        }
    }
    probe.insert("written_at".to_owned(), "2026-01-01T00:00Z".into());
    probe.insert("written_epoch".to_owned(), 1_700_000_000_u64.into());
    let n = serde_json::to_string(&probe).map_or(usize::MAX, |s| s.len());
    if n <= SESSION_MAX {
        return Ok(());
    }
    let fields: Vec<String> = written
        .iter()
        .map(|(f, b)| format!("{f} is {b} B"))
        .collect();
    Err(format!(
        "the Intent Ledger session record would be {n} B with the holds and its limit is \
         {SESSION_MAX} B ({})",
        fields.join(", ")
    ))
}

/// The active holds in `local/state.md`; see the module doc.
pub(crate) fn holds_in_state_md(text: &str) -> Result<Holds, (usize, String)> {
    let mut found = Holds::default();
    for (line, lines) in entries(text) {
        let all = lines.join("\n");
        let h = head(lines[0]);
        // #11480: only an entry whose head starts with a hold format is a hold;
        // a note that merely uses the word "hold" or "merge" is neither.
        if !starts_with_hold_format(h) || historical(lines[0], &all) {
            continue;
        }
        let low = h.to_ascii_lowercase();
        let usage = low
            .strip_prefix("usage state:")
            .map(|_| h["usage state:".len()..].trim())
            .filter(|v| v.get(..4).is_some_and(|w| w.eq_ignore_ascii_case("hold")));
        let plain = h
            .get(..4)
            .filter(|w| w.eq_ignore_ascii_case("hold"))
            .map(|_| h[4..].trim_start_matches('*').trim_start())
            .and_then(|r| r.strip_prefix(':'))
            .map(str::trim);
        if let Some(v) = usage.or(plain) {
            if !v.is_empty() && !v.eq_ignore_ascii_case("none") && found.usage.is_none() {
                found.usage = Some(value(v, &lines[1..]));
            }
            continue;
        }
        if all.to_ascii_lowercase().contains("merge") {
            let entry = value(h, &lines[1..]);
            found.merge = Some(match found.merge.take() {
                // #11498: never shortened; `hold_room` refuses an over-long hold.
                Some(prev) => format!("{prev} || {entry}"),
                None => entry,
            });
            continue;
        }
        return Err((line, lines[0].trim().chars().take(120).collect()));
    }
    Ok(found)
}
