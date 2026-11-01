//! The holds recorded in an Architect's `local/state.md` (#10579).
//!
//! Why: a lost hold silently releases what the owner held, and a lifted or
//! historical one carried by mistake blocks work. The whole file is read, each
//! entry classified, and anything unclear stops the migration.
//! What: [`holds_in_state_md`] splits the file into entries (a line and the
//! indented lines under it) and classifies every entry that names a hold (the
//! word "hold" in any case): lifted or released, a recorded builder band, or an
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

/// Whether `s` names a hold: the word "hold" in any case (#10579: lowercase
/// "hold all merges" is a hold too).
fn names_hold(s: &str) -> bool {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("hold"))
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
    parts.join(" | ").chars().take(HOLD_MAX).collect()
}

/// The active holds in `local/state.md`; see the module doc.
pub(crate) fn holds_in_state_md(text: &str) -> Result<Holds, (usize, String)> {
    let mut found = Holds::default();
    for (line, lines) in entries(text) {
        let all = lines.join("\n");
        if !names_hold(&all) || historical(lines[0], &all) {
            continue;
        }
        let h = head(lines[0]);
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
                Some(prev) => format!("{prev} || {entry}")
                    .chars()
                    .take(HOLD_MAX)
                    .collect(),
                None => entry,
            });
            continue;
        }
        return Err((line, lines[0].trim().chars().take(120).collect()));
    }
    Ok(found)
}
