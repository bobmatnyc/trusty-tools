//! `ilio check`: validate an IL block against the ledger.
//!
//! Why: the block is the model's only write path, so every field is checked
//! before anything is written: grammar, required fields, order, line length,
//! secrets, `pct`, reminders, and every id against the ledger (stale, re-issued,
//! look-alike). The reject text `IL-REJECT <CODE> <field>: <expected>` is what the
//! model reads and retries on, so it must match `ilio.py` exactly.
//! What: [`check`] returns `None` for a valid block, else the reject line. The
//! codes: NO-BLOCK, MISSING, ORDER, FIELD, LONG, SECRET, PCT, REMIND, BADID,
//! STALE-ID, RREF. [`parse_remind`] is the `REMIND` regex.
//! Test: `golden_steps_match_python_byte_for_byte` (every reject code), `check_matches_python_on_more_blocks`.

use super::idre::{id_full, ids_in, lookalike_full, rrefs_in};
use super::secret::looks_secret;
use super::{FIELDS, Lib, REQUIRED, ledger_ids, parse_block, relay_text, task_id};
use crate::il::json::Json;
use crate::il::remind::{note, parse_at, parse_every};
use crate::pystr::{is_space, parse_digits};

const LINE_MAX: usize = 240;
const NEW_LINE_MAX: usize = 1200;

/// The groups of `REMIND`: `<task> at=<ts> [every=14d] "note"`.
pub struct RemindLine {
    /// The task token.
    pub task: String,
    /// The `at=` value.
    pub at: String,
    /// The `every=` value.
    pub every: Option<String>,
    /// The quoted note.
    pub note: String,
}

/// `REMIND.match(v)`.
pub fn parse_remind(v: &str) -> Option<RemindLine> {
    let t: Vec<char> = v.chars().collect();
    let mut i = 0;
    let word = |i: &mut usize| {
        let n = t[*i..].iter().take_while(|c| !is_space(**c)).count();
        let w: String = t[*i..*i + n].iter().collect();
        *i += n;
        (n > 0).then_some(w)
    };
    let ws1 = |i: &mut usize| {
        let n = t[*i..].iter().take_while(|c| is_space(**c)).count();
        *i += n;
        n > 0
    };
    let task = word(&mut i)?;
    if !ws1(&mut i) || !t[i..].starts_with(&['a', 't', '=']) {
        return None;
    }
    i += 3;
    let at = word(&mut i)?;
    if !ws1(&mut i) {
        return None;
    }
    let mut every = None;
    if t[i..].starts_with(&['e', 'v', 'e', 'r', 'y', '=']) {
        i += 6;
        every = Some(word(&mut i)?);
        if !ws1(&mut i) {
            return None;
        }
    }
    if t.get(i) != Some(&'"') || t.last() != Some(&'"') || t.len() < i + 3 {
        return None;
    }
    let body: Vec<char> = t[i + 1..t.len() - 1].to_vec();
    if body.contains(&'"') {
        return None;
    }
    Some(RemindLine {
        task,
        at,
        every,
        note: body.into_iter().collect(),
    })
}

/// `remind_problem`: `None` when a `remind:` value is valid, else the expected form or the reason.
fn remind_problem(v: &str, st: &Json) -> Option<String> {
    let Some(m) = parse_remind(v) else {
        return Some("form <task> at=<ts> [every=14d] \"note\"".to_owned());
    };
    if task_id(st, &m.task).is_none() {
        return Some(format!("{} is not a live task in the ledger", m.task));
    }
    parse_at(&m.at)
        .and_then(|_| parse_every(m.every.as_deref().unwrap_or("")))
        .and_then(|_| note(&m.note))
        .err()
}

fn reject(code: &str, field: &str, expected: &str) -> Option<String> {
    Some(format!("IL-REJECT {code} {field}: {expected}"))
}

fn split_ids(v: &str) -> Vec<&str> {
    v.split([' ', ',']).collect()
}

fn pct_ok(v: &str) -> bool {
    let chars: Vec<char> = v.chars().collect();
    !chars.is_empty() && parse_digits(&chars).is_some_and(|n| n <= 100)
}

/// Checks the ids of a `task`, `open` or `close` field.
fn check_ids(k: &str, v: &str, st: &Json) -> Option<String> {
    for tok in split_ids(v) {
        if !tok.is_empty() && lookalike_full(tok) && id_full(tok).is_none() {
            return reject(
                "BADID",
                k,
                &format!("{tok} is not base 34 (no o, no l; did you mean 0 or 1)"),
            );
        }
    }
    let ids = ledger_ids(st);
    let chars: Vec<char> = v.chars().collect();
    for m in ids_in(&chars) {
        if m.pre == 'R' {
            continue;
        }
        let shown: String = chars[m.start..m.end].iter().collect();
        let full = m.code.as_ref().map(|c| format!("{c}-{}{}", m.pre, m.n));
        let suffix = format!("-{}{}", m.pre, m.n);
        let hit = full
            .as_ref()
            .and_then(|f| ids.iter().find(|(id, ..)| id == f))
            .or_else(|| ids.iter().find(|(id, ..)| id.ends_with(&suffix)));
        let Some(hit) = hit else {
            return reject("STALE-ID", k, &format!("{shown} is not in the ledger"));
        };
        if let (Some(stamp), Some(holder)) = (&m.stamp, &hit.3)
            && stamp != holder
        {
            return reject(
                "STALE-ID",
                k,
                &format!("{shown} was re-issued; holder created @{holder}"),
            );
        }
    }
    None
}

/// `check`: `None` when the block is valid, else `IL-REJECT <code> <field>: <expected>` (at most 200 characters for REMIND).
pub fn check(text: &str, st: &Json, lib: &Lib) -> Option<String> {
    let Some(rows) = parse_block(text) else {
        return reject("NO-BLOCK", "il", "end the turn with one ```il fenced block");
    };
    let names: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    for want in REQUIRED {
        if !names.contains(&want) {
            return reject(
                "MISSING",
                want,
                &format!("required fields {}", REQUIRED.join(",")),
            );
        }
    }
    let order: Vec<usize> = names
        .iter()
        .filter_map(|k| FIELDS.iter().position(|f| f == k))
        .collect();
    if order.windows(2).any(|w| w[0] > w[1]) {
        return reject(
            "ORDER",
            names[0],
            &format!("fields in the order {}...", FIELDS[..10].join(",")),
        );
    }
    for (k, v) in &rows {
        let k = k.as_str();
        if !FIELDS.contains(&k) {
            return reject(
                "FIELD",
                k,
                &format!("unknown field; allowed {}", FIELDS.join(",")),
            );
        }
        let cap = if k == "new" { NEW_LINE_MAX } else { LINE_MAX };
        if format!("{k}: {v}").chars().count() > cap {
            return reject("LONG", k, &format!("line over {cap} characters"));
        }
        if looks_secret(v) {
            return reject(
                "SECRET",
                k,
                "a secret-shaped value; record its location only",
            );
        }
        if k == "pct" && !pct_ok(v) {
            return reject("PCT", "pct", "an integer 0-100");
        }
        if k == "remind" {
            if let Some(bad) = remind_problem(v, st) {
                let line: String = format!("IL-REJECT REMIND remind: {bad}")
                    .chars()
                    .take(200)
                    .collect();
                return Some(line);
            }
            continue;
        }
        if k == "goal"
            || (matches!(k, "task" | "open" | "close" | "done" | "next") && v.starts_with("free:"))
        {
            continue;
        }
        if matches!(k, "task" | "open" | "close")
            && let Some(r) = check_ids(k, v, st)
        {
            return Some(r);
        }
        if k == "done" {
            for r in rrefs_in(v) {
                if relay_text(&r, &[], lib).is_none() {
                    return reject("RREF", "done", &format!("{r} is not in the R library"));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remind_line_grammar() {
        let m = parse_remind(r#"ar-T0f at=2026-11-20 every=14d "ping Bob""#).unwrap();
        assert_eq!(
            (
                m.task.as_str(),
                m.at.as_str(),
                m.every.as_deref(),
                m.note.as_str()
            ),
            ("ar-T0f", "2026-11-20", Some("14d"), "ping Bob")
        );
        assert!(parse_remind(r#"ar-T0f at=2026-11-20 "n""#).is_some());
        for bad in [
            r#"ar-T0f at=x"#,
            r#"ar-T0f at=x """#,
            r#"ar-T0f at=x "a"b""#,
            r#"ar-T0f x=1 "n""#,
            "",
        ] {
            assert!(parse_remind(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn pct_accepts_zero_to_hundred() {
        for ok in ["0", "7", "100"] {
            assert!(pct_ok(ok), "{ok}");
        }
        for no in ["", "101", "-1", "5%", "x"] {
            assert!(!pct_ok(no), "{no}");
        }
    }
}
