//! The port of `task_wait_check.py`: what open ledger tasks say they wait on (#12028 P1d).
//!
//! Why: a task's `remaining` or `waiting_on` kept naming a dashboard item
//! after the owner answered it, and some `done_when` lines held nothing
//! checkable; the advisory pass names both.
//! What: [`check`] prints, per open or running task, one `STALE-WAIT` line
//! for each resolved item (`answered`, `relayed`, `done`, `closed`,
//! `wont_do`, `cancelled`) its `remaining` or `waiting_on` names by a
//! dashboard id (`D`/`A`/`B` and two characters) or an IL id (`ar-D..`),
//! looked up by file name or `il` field, and one `UNCHECKABLE` line when
//! every `done_when` clause is an owner confirmation or it has under three
//! words. [`run`] is `main()`: the lines or `none`, any failure as one
//! `task_wait_check failed: <class>: <text>` line, exit 0 always.
//! Known gap: items are read in directory order, so two files claiming one
//! id resolve to whichever the platform lists last, as in the script.
//! Test: `task_wait_check_recordings_replay_clean_against_the_port`,
//! `shipped_task_commands_read_the_working_directory_ledger`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::pyjson_load::PyValue;
use crate::pystr::{is_space, is_word, strip};
use crate::pyval::{dumps, get, py_str, truthy, type_name};
use crate::spec_drift::Outcome;

use super::{Ledger, read_json};

const RESOLVED: [&str; 6] = [
    "answered",
    "relayed",
    "done",
    "closed",
    "wont_do",
    "cancelled",
];
const NOT_IDS: [&str; 10] = [
    "Bob", "Dan", "Add", "Bug", "Doc", "Dev", "App", "Apr", "Aug", "Dec",
];
const DONE_WORDS: [&str; 8] = [
    "confirms",
    "confirm",
    "approves",
    "approve",
    "ok",
    "okays",
    "signs off",
    "says",
];

/// The dashboard items by file-name id and by `il` id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Items(HashMap<String, PyValue>);

/// Why: a wait is stale only when the item it names is resolved.
/// What: `load_items(dir)`: each `*.json` dict under its file-name id and,
/// when its `il` is set, under `str(il)`; an unreadable file or directory is
/// skipped as the script does.
/// Test: the `stale-waits` recording.
pub fn load_items(dir: &Path) -> Items {
    let mut out = HashMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Items(out);
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let Ok(j @ PyValue::Dict(_)) = read_json(&dir.join(&name)) else {
            continue;
        };
        let il = get(&j, "il").filter(|v| truthy(v)).map(py_str);
        out.insert(stem.to_owned(), j.clone());
        if let Some(il) = il {
            out.insert(il, j);
        }
    }
    Items(out)
}

fn alnum(c: Option<&char>) -> bool {
    c.is_some_and(char::is_ascii_alphanumeric)
}

/// `ID_RE.finditer(text)`: the ids, left to right, without overlap.
fn ids(text: &str) -> Vec<String> {
    let c: Vec<char> = text.chars().collect();
    let dab = |i: usize| matches!(c.get(i), Some('D' | 'A' | 'B'));
    let run = |from: usize| (from..c.len()).take_while(|k| alnum(c.get(*k))).count();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let behind_ok = i == 0 || !(alnum(c.get(i - 1)) || c[i - 1] == '-');
        let len = if !behind_ok {
            None
        } else if c[i..].starts_with(&['a', 'r', '-']) && dab(i + 3) && matches!(run(i + 4), 2 | 3)
        {
            Some(4 + run(i + 4))
        } else if dab(i) && run(i + 1) == 2 {
            Some(3)
        } else {
            None
        };
        match len {
            Some(n) => {
                out.push(c[i..i + n].iter().collect());
                i += n;
            }
            None => i += 1,
        }
    }
    out
}

/// `_text(v)`.
fn text(v: Option<&PyValue>) -> String {
    match v {
        Some(v @ (PyValue::List(_) | PyValue::Dict(_))) => dumps(v, None, false),
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    }
}

/// `(field, id, status, when)` for each resolved item the task names.
fn stale_waits(t: &PyValue, items: &Items) -> Vec<(&'static str, String, String, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for field in ["remaining", "waiting_on"] {
        for i in ids(&text(get(t, field))) {
            let Some(it) = items.0.get(&i).filter(|it| truthy(it)) else {
                continue;
            };
            let status = match get(it, "status") {
                Some(PyValue::Str(s)) if RESOLVED.contains(&s.as_str()) => s.clone(),
                _ => continue,
            };
            if NOT_IDS.contains(&i.as_str()) || !seen.insert(i.clone()) {
                continue;
            }
            let when = ["answered_at", "updated"]
                .iter()
                .find_map(|k| get(it, k).filter(|v| truthy(v)));
            let when: String = when
                .map_or(String::new(), py_str)
                .chars()
                .take(16)
                .collect();
            out.push((field, i, status, format!("{when}Z")));
        }
    }
    out
}

/// `re.IGNORECASE` match of one text character against a lower-case ASCII
/// pattern letter, with `re`'s extra equivalences (`ı`/`İ`, `ſ`, Kelvin).
fn ci_eq(c: char, p: char) -> bool {
    c.to_ascii_lowercase() == p
        || (p == 'i' && matches!(c, '\u{130}' | '\u{131}'))
        || (p == 's' && c == '\u{17f}')
        || (p == 'k' && c == '\u{212a}')
}

/// `word` at `c[at..]` (ignoring case) followed by `\b`.
fn word_at(c: &[char], at: usize, word: &str) -> Option<usize> {
    let w: Vec<char> = word.chars().collect();
    let end = at + w.len();
    let ok = c
        .get(at..end)
        .is_some_and(|s| s.iter().zip(&w).all(|(x, p)| ci_eq(*x, *p)));
    (ok && c.get(end).is_none_or(|n| !is_word(*n))).then_some(end)
}

/// `OWNER_CLAUSE.match(clause)`.
fn owner_clause(clause: &str) -> bool {
    let c: Vec<char> = clause.chars().collect();
    let start = c.iter().take_while(|x| is_space(**x)).count();
    let Some(after) = word_at(&c, start, "bob").or_else(|| word_at(&c, start, "owner")) else {
        return false;
    };
    (after..c.len())
        .any(|p| !is_word(c[p - 1]) && DONE_WORDS.iter().any(|w| word_at(&c, p, w).is_some()))
}

/// `uncheckable(task)`.
fn uncheckable(t: &PyValue) -> bool {
    let raw = get(t, "done_when")
        .filter(|v| truthy(v))
        .map_or(String::new(), py_str);
    let dw = strip(&raw);
    if dw.is_empty() {
        return false;
    }
    let all_owner = dw
        .split([';', '\n'])
        .filter(|c| !strip(c).is_empty())
        .all(owner_clause);
    all_owner || dw.split(is_space).filter(|w| !w.is_empty()).count() < 3
}

fn id_of(t: &PyValue) -> Result<String, String> {
    get(t, "id")
        .map(py_str)
        .ok_or_else(|| "KeyError: 'id'".to_owned())
}

/// Why: the advisory lines for every open or running task.
/// What: see the module doc; `Err` is `<class>: <text>` where Python raises
/// (a non-dict task, a task with no `id`).
/// Test: the `stale-waits`, `task-not-dict` and `task-no-id` recordings.
///
/// # Errors
/// The exception `main()` prints.
pub fn check(ledger: &Ledger, items: &Items) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for t in ledger.tasks() {
        if !matches!(t, PyValue::Dict(_)) {
            return Err(format!(
                "AttributeError: '{}' object has no attribute 'get'",
                type_name(t)
            ));
        }
        let st = get(t, "status");
        if !matches!(st, Some(PyValue::Str(s)) if s == "open" || s == "running") {
            continue;
        }
        for (field, i, status, when) in stale_waits(t, items) {
            lines.push(format!(
                "STALE-WAIT {} names {i} in {field}, but {i} is {status} ({when})",
                id_of(t)?
            ));
        }
        if uncheckable(t) {
            let dw: String = get(t, "done_when")
                .map_or_else(|| "None".to_owned(), py_str)
                .chars()
                .take(80)
                .collect();
            lines.push(format!(
                "UNCHECKABLE {} done_when has no observable: \"{dw}\"",
                id_of(t)?
            ));
        }
    }
    Ok(lines)
}

/// Why: the script's `main()`, run from the Architect checkout.
/// What: [`Ledger::load`] on `il_dir`, [`load_items`] on `items_dir`, then
/// [`check`]; prints the lines or `none`; exit 0.
/// Test: `task_wait_check_recordings_replay_clean_against_the_port`.
pub fn run(il_dir: &Path, items_dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> Outcome {
    let lines = match Ledger::load(il_dir, env) {
        Err(e) => vec![format!("task_wait_check failed: DatgError: {e}")],
        Ok(ledger) => match check(&ledger, &load_items(items_dir)) {
            Ok(lines) => lines,
            Err(e) => vec![format!("task_wait_check failed: {e}")],
        },
    };
    let body = if lines.is_empty() {
        "none".to_owned()
    } else {
        lines.join("\n")
    };
    Outcome {
        stdout: format!("{body}\n"),
        stderr: String::new(),
        code: 0,
    }
}
