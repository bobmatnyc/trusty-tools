//! `aliases.py`'s `check(table)`: schema errors and shared aliases (#12028 P1e).
//!
//! Why: `add` refuses to write a table that fails this check, and `check`
//! reports it, so both must flag exactly what the script flags.
//! What: [`check`] returns `(errors, notes)` in the script's order: per kind
//! and entry the missing or mistyped fields, list items that are not
//! non-empty strings, an email-shaped value in the entry's `json.dumps`;
//! then every alias (as tokens, sorted) that two entries share, an error
//! within one kind and a note across kinds. `Err` is the `TypeError` the
//! script raises on a table it cannot iterate.
//! Test: the `aliases/check-*` and `aliases/add-refused` recordings.

use std::collections::{BTreeMap, BTreeSet};

use super::{KINDS, entries, name, norm, str_items};
use crate::pyjson_load::PyValue;
use crate::pyval::{dumps, get};

/// A field's accepted type.
#[derive(Clone, Copy)]
enum Ty {
    Str,
    List,
    Bool,
    StrOrNone,
}

const COMMON: [(&str, Ty); 4] = [
    ("name", Ty::Str),
    ("aliases", Ty::List),
    ("source", Ty::Str),
    ("confirmed", Ty::Bool),
];

fn fields(kind: &str) -> &'static [(&'static str, Ty)] {
    match kind {
        "person" => &[
            ("github", Ty::List),
            ("slack_user_id", Ty::StrOrNone),
            ("email_location", Ty::Str),
            ("role", Ty::Str),
        ],
        "project" => &[
            ("sessions", Ty::List),
            ("repo_path", Ty::StrOrNone),
            ("github_repo", Ty::StrOrNone),
            ("palace", Ty::StrOrNone),
            ("index_id", Ty::StrOrNone),
        ],
        _ => &[("hostname", Ty::StrOrNone), ("role", Ty::Str)],
    }
}

/// `not isinstance(v, t) or (t is str and not v.strip())`.
fn bad(v: &PyValue, t: Ty) -> bool {
    match (t, v) {
        (Ty::Str, PyValue::Str(s)) => crate::pystr::strip(s).is_empty(),
        (Ty::List, PyValue::List(_)) | (Ty::Bool, PyValue::Bool(_)) => false,
        (Ty::StrOrNone, PyValue::Str(_) | PyValue::Null) => false,
        _ => true,
    }
}

/// `_EMAIL.search(s)` (`[^\s@]+@[^\s@]+\.[a-z]{2,}`, caseless) on the ASCII
/// text `json.dumps` writes; each `@` scans only to the next `@` or space.
fn has_email(s: &str) -> bool {
    let b = s.as_bytes();
    let part =
        |c: u8| c != b'@' && !(c.is_ascii_whitespace() || (0x1c..=0x1f).contains(&c) || c == 0x0b);
    (1..b.len()).any(|at| {
        if b[at] != b'@' || !part(b[at - 1]) {
            return false;
        }
        let run_end = (at + 1..b.len()).find(|&i| !part(b[i])).unwrap_or(b.len());
        (at + 2..run_end).any(|d| {
            b[d] == b'.'
                && b.get(d + 1).is_some_and(u8::is_ascii_alphabetic)
                && b.get(d + 2).is_some_and(u8::is_ascii_alphabetic)
        })
    })
}

/// `check(table)`: `(errors, notes)`, or the `TypeError` line it raises.
pub(crate) fn check(table: &PyValue) -> Result<(Vec<String>, Vec<String>), String> {
    let (mut errs, mut notes) = (Vec::new(), Vec::new());
    if !matches!(table, PyValue::Dict(_)) {
        return Ok((vec!["table is not an object".into()], notes));
    }
    for (kind, key) in KINDS {
        let rows = match get(table, key) {
            None => continue,
            Some(PyValue::List(rows)) => rows,
            Some(_) => {
                errs.push(format!("{key}: not a list"));
                continue;
            }
        };
        for (n, e) in rows.iter().enumerate() {
            let mut tag = format!("{key}[{n}]");
            if let Some(PyValue::Str(nm)) = get(e, "name") {
                tag += &format!(" {nm}");
            }
            if !matches!(e, PyValue::Dict(_)) {
                errs.push(format!("{tag}: not an object"));
                continue;
            }
            for (f, t) in COMMON.iter().chain(fields(kind)) {
                match get(e, f) {
                    None => errs.push(format!("{tag}: missing {f}")),
                    Some(v) if bad(v, *t) => errs.push(format!("{tag}: bad type or empty {f}")),
                    Some(_) => {}
                }
            }
            for f in ["aliases", "github", "sessions", "quiet"] {
                if let Some(PyValue::List(items)) = get(e, f)
                    && !items
                        .iter()
                        .all(|x| matches!(x, PyValue::Str(s) if !crate::pystr::strip(s).is_empty()))
                {
                    errs.push(format!("{tag}: {f} holds a non-string or empty item"));
                }
            }
            if has_email(&dumps(e, None, true)) {
                errs.push(format!(
                    "{tag}: contains an email address (store a location note instead)"
                ));
            }
        }
    }
    let mut owners: BTreeMap<Vec<String>, BTreeSet<(&str, String)>> = BTreeMap::new();
    for (kind, e) in entries(table)? {
        let mut names = vec![name(e).to_owned()];
        names.extend(str_items(get(e, "aliases"))?);
        for a in names {
            let k = norm(&a);
            if !k.is_empty() {
                owners
                    .entry(k)
                    .or_default()
                    .insert((kind, name(e).to_owned()));
            }
        }
    }
    for (a, who) in &owners {
        if who.len() < 2 {
            continue;
        }
        let list: Vec<String> = who.iter().map(|(k, n)| format!("{n} ({k})")).collect();
        let label = format!("{}: {}", a.join(" "), list.join(" | "));
        let kinds: BTreeSet<&str> = who.iter().map(|(k, _)| *k).collect();
        if kinds.len() < who.len() {
            errs.push(format!("duplicate alias, same kind: {label}"));
        } else {
            notes.push(format!("ambiguous alias: {label}"));
        }
    }
    Ok((errs, notes))
}

#[cfg(test)]
mod tests {
    use super::has_email;

    #[test]
    fn email_search_matches_python() {
        // Expected values from Python 3.14 `_EMAIL.search(...) is not None`.
        assert!(has_email("{\"x\": \"a@example.com\"}"));
        assert!(has_email("x@y.example.COM"));
        assert!(!has_email("@b.co"));
        assert!(!has_email("a@.co"));
        assert!(!has_email("a@b.c"));
        assert!(has_email("a@b@example.com"));
        assert!(!has_email("a @b.co"));
    }
}
