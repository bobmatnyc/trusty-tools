//! The port of `stalled_tasks.py`: the STALLED and OWNER WAIT lines (#12028 P1d).
//!
//! Why: every Architect turn names open ledger tasks with no progress past
//! their priority limit, and owner waits nobody asked the owner about, until
//! progress is written; the hook text must match the script's.
//! What: [`line`] lists open or running tasks, not waiting on the owner, whose
//! last progress (`progress_at`, else `created`) is older than the limit (P1
//! 2 h, P2 4 h, else 12 h), most over the limit first, at most two, then
//! `+N more`. [`unasked_line`] lists the owner waits past their limit that no
//! open dashboard ask names (its `task` field, or the id in its title or
//! detail), oldest first, at most `cap`. Each returns `None` when nothing
//! qualifies or where the script's `except Exception` returns `None` (a
//! non-dict task, a missing id, an infinite priority). [`run`] is the
//! script's command line: both lines, `none` for an empty one.
//! Test: `stalled_tasks_recordings_replay_clean_against_the_port`,
//! `shipped_task_commands_read_the_working_directory_ledger`.

use std::path::Path;

use crate::Clock;
use crate::dashboard::derive::now_micros;
use crate::dashboard::pending::from_iso;
use crate::poller::pyfmt::py_int_text;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::{is_space, is_word, strip};
use crate::pystrptime::strptime;
use crate::pyval::{get, int_text, is_str, py_str, truthy};
use crate::spec_drift::Outcome;

use super::{JsonFail, Ledger, read_json};

const MAX_TASKS: usize = 2;

/// The open asks to the owner: dashboard items that are not tasks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Asks(Vec<PyValue>);

/// Why: an owner wait counts as asked only when an open ask names it.
/// What: `load_open_asks(dir)`: every `*.json` in `dir` that is a dict with
/// `kind` not `task` and `status` `open` or `brief_pending`; a missing or
/// unlistable directory, or an unreadable file, is skipped as the script does.
/// Test: the `cli-stalled` and `hook-form` recordings.
pub fn load_open_asks(dir: &Path) -> Asks {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Asks::default();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") {
            continue;
        }
        let j = match read_json(&dir.join(&name)) {
            Ok(j) => j,
            Err(JsonFail::NotFound | JsonFail::Error(_)) => continue,
        };
        let status = get(&j, "status");
        if matches!(j, PyValue::Dict(_))
            && !is_str(get(&j, "kind"), "task")
            && (is_str(status, "open") || is_str(status, "brief_pending"))
        {
            out.push(j);
        }
    }
    Asks(out)
}

/// `_ts(s)`: `fromisoformat`, else `strptime("%Y-%m-%dT%H:%M%z")`, as UTC microseconds.
fn ts(v: Option<&PyValue>) -> Option<i64> {
    let raw = v.filter(|v| truthy(v)).map_or(String::new(), py_str);
    let s = strip(&raw).replace('Z', "+00:00");
    from_iso(&s).or_else(|| strptime(&s, "%Y-%m-%dT%H:%M%z").map(|(l, o)| l - o.unwrap_or(0)))
}

/// True when `word` occurs in `text` between `\b` boundaries.
fn has_word(text: &[char], word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    (0..text.len()).any(|i| {
        text[i..].starts_with(&w)
            && (i == 0 || !is_word(text[i - 1]))
            && text.get(i + w.len()).is_none_or(|c| !is_word(*c))
    })
}

/// `_owner_wait(t)`.
fn owner_wait(t: &PyValue) -> bool {
    if let Some(w) = get(t, "waiting_on").filter(|w| truthy(w)) {
        let blob: Vec<char> = py_str(w).to_lowercase().chars().collect();
        if has_word(&blob, "owner") || has_word(&blob, "bob") {
            return true;
        }
    }
    // `re.match(r"\s*bob\b", remaining, re.I)`.
    let rem: Vec<char> = get(t, "remaining")
        .filter(|v| truthy(v))
        .map_or(String::new(), py_str)
        .chars()
        .collect();
    let start = rem.iter().take_while(|c| is_space(**c)).count();
    let head: String = rem.iter().skip(start).take(3).collect();
    head.eq_ignore_ascii_case("bob") && rem.get(start + 3).is_none_or(|c| !is_word(*c))
}

/// `int(t.get("priority"))` with `TypeError`/`ValueError` as 0; `Err` where
/// Python raises something else (`OverflowError` on an infinite float).
fn priority(t: &PyValue) -> Result<PyInt, ()> {
    Ok(match get(t, "priority") {
        Some(PyValue::Int(i)) => i.clone(),
        Some(PyValue::Bool(b)) => PyInt::Small(i64::from(*b)),
        Some(PyValue::Float(f)) if f.is_nan() => PyInt::Small(0),
        Some(PyValue::Float(f)) if f.is_infinite() => return Err(()),
        Some(PyValue::Float(f)) if f.abs() < 9e18 => PyInt::Small(f.trunc() as i64),
        Some(PyValue::Float(f)) => PyInt::Big(format!("{:.0}", f.trunc())),
        Some(PyValue::Str(s)) => match py_int_text(s) {
            Some(t) => {
                let t = t.trim_start_matches('+').to_owned();
                t.parse().map_or(PyInt::Big(t), PyInt::Small)
            }
            None => PyInt::Small(0),
        },
        _ => PyInt::Small(0),
    })
}

fn limit_h(p: &PyInt) -> f64 {
    match p {
        PyInt::Small(1) => 2.0,
        PyInt::Small(2) => 4.0,
        _ => 12.0,
    }
}

fn label(p: &PyInt) -> String {
    match p {
        PyInt::Small(0) => "P-".into(),
        p => format!("P{}", int_text(p)),
    }
}

/// `str(v or w or "unrecorded").replace("\n", " ")[:60]`.
fn wait_text(t: &PyValue, first: &str, second: &str) -> String {
    let v = [first, second]
        .iter()
        .find_map(|k| get(t, k).filter(|v| truthy(v)));
    let s = v
        .map_or_else(|| "unrecorded".to_owned(), py_str)
        .replace('\n', " ");
    s.chars().take(60).collect()
}

/// One candidate row: the sort key, the task, its priority and age in hours.
type Cand<'a> = (f64, &'a PyValue, PyInt, f64);

/// The tasks past their limit, or `Err` where the script's body raises.
fn over_limit<'a>(
    ledger: &'a Ledger,
    now: f64,
    keep: &dyn Fn(&PyValue) -> bool,
    excess: bool,
) -> Result<Vec<Cand<'a>>, ()> {
    let now_us = now_micros(now);
    let mut rows = Vec::new();
    for t in ledger.tasks() {
        if !matches!(t, PyValue::Dict(_)) {
            return Err(()); // `t.get` on a non-dict raises AttributeError
        }
        let st = get(t, "status");
        if !(is_str(st, "open") || is_str(st, "running")) || !keep(t) {
            continue;
        }
        let Some(at) = ts(get(t, "progress_at")).or_else(|| ts(get(t, "created"))) else {
            continue;
        };
        let p = priority(t)?;
        let limit = limit_h(&p);
        let age = (now_us - at) as f64 / 1e6 / 3600.0;
        if age > limit {
            rows.push((if excess { age - limit } else { age }, t, p, age));
        }
    }
    // `sort(key=lambda r: -r[0])`, stable.
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(rows)
}

fn shown_rows(
    rows: &[Cand<'_>],
    cap: Option<usize>,
    first: &str,
    second: &str,
) -> Option<Vec<String>> {
    let shown = cap.map_or(rows.len(), |c| c.min(rows.len()));
    let mut out = Vec::new();
    for (_, t, p, age) in &rows[..shown] {
        let id = get(t, "id")?; // `t['id']` raises KeyError
        out.push(format!(
            "{} {} {age:.1}h waits: {}",
            py_str(id),
            label(p),
            wait_text(t, first, second)
        ));
    }
    if shown < rows.len() {
        out.push(format!("+{} more", rows.len() - shown));
    }
    Some(out)
}

/// Why: the STALLED line of the Architect's prompt hook.
/// What: see the module doc; `clock` gives now.
/// Test: `stalled_tasks_recordings_replay_clean_against_the_port`.
pub fn line(ledger: &Ledger, clock: &dyn Clock) -> Option<String> {
    let rows = over_limit(ledger, clock.now_unix(), &|t| !owner_wait(t), true).ok()?;
    if rows.is_empty() {
        return None;
    }
    let out = shown_rows(&rows, Some(MAX_TASKS), "remaining", "to")?;
    Some(format!(
        "STALLED (no progress past priority limit): {}",
        out.join(" | ")
    ))
}

fn bare(v: Option<&PyValue>) -> String {
    let s = v.filter(|v| truthy(v)).map_or(String::new(), py_str);
    s.strip_prefix("ar-").map_or(s.clone(), str::to_owned)
}

/// `(?<![A-Za-z0-9])(ar-)?<b>(?![A-Za-z0-9])` found in `text`.
fn names_id(text: &str, b: &str) -> bool {
    let alnum = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    let mut from = 0;
    while let Some(off) = text[from..].find(b) {
        let i = from + off;
        if !alnum(text[..i].chars().next_back()) && !alnum(text[i + b.len()..].chars().next()) {
            return true;
        }
        from = i + text[i..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// `_asked(task, asks)`.
fn asked(t: &PyValue, asks: &Asks) -> bool {
    let b = bare(get(t, "id"));
    if b.is_empty() {
        return false;
    }
    let field = |a: &PyValue, k: &str| get(a, k).map_or(String::new(), py_str);
    asks.0.iter().any(|a| {
        bare(get(a, "task")) == b
            || names_id(&format!("{} {}", field(a, "title"), field(a, "detail")), &b)
    })
}

/// Why: the OWNER WAIT, NEVER ASKED line the hook appends (`cap` 2) and the
/// pass prints in full (`cap` `None`).
/// What: see the module doc.
/// Test: the `cli-stalled` and `hook-form` recordings.
pub fn unasked_line(
    ledger: &Ledger,
    asks: &Asks,
    clock: &dyn Clock,
    cap: Option<usize>,
) -> Option<String> {
    let keep = |t: &PyValue| owner_wait(t) && !asked(t, asks);
    let rows = over_limit(ledger, clock.now_unix(), &keep, false).ok()?;
    if rows.is_empty() {
        return None;
    }
    let out = shown_rows(&rows, cap, "remaining", "waiting_on")?;
    Some(format!(
        "OWNER WAIT, NEVER ASKED (no open D/A item): {}",
        out.join(" | ")
    ))
}

/// Why: the script's command line, run from the Architect checkout.
/// What: `datg.load_state(il_dir)` (a failure prints the traceback's last
/// line, exit 1), then [`line`] and [`unasked_line`] with `cap`, each line
/// or `none`.
/// Test: `stalled_tasks_recordings_replay_clean_against_the_port`.
pub fn run(
    il_dir: &Path,
    items_dir: &Path,
    cap: Option<usize>,
    env: &dyn Fn(&str) -> Option<String>,
    clock: &dyn Clock,
) -> Outcome {
    let ledger = match Ledger::load(il_dir, env) {
        Ok(l) => l,
        Err(e) => {
            return Outcome {
                stdout: String::new(),
                stderr: format!("datg.DatgError: {e}\n"),
                code: 1,
            };
        }
    };
    let asks = load_open_asks(items_dir);
    let mut stdout = String::new();
    for ln in [
        line(&ledger, clock),
        unasked_line(&ledger, &asks, clock, cap),
    ] {
        stdout += &ln.unwrap_or_else(|| "none".into());
        stdout.push('\n');
    }
    Outcome {
        stdout,
        stderr: String::new(),
        code: 0,
    }
}
