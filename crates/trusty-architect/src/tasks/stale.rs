//! The port of `stale-tasks.py` and the `stale_ranked` rule of `dashboard_derive.py` (#12028 P1d).
//!
//! Why: the Architect parks idle dashboard tasks; the list it reads must name
//! the same tasks, in the same order, with the same text as the script.
//! What: [`ranked`] is `stale_ranked(items, goals, now, hours)`: open,
//! unparked, non-factory tasks whose last real progress (`progress_at`,
//! `log[].at`, `reported.at`, else legacy `progress.estimated_at`, else
//! `created`) is missing or before `now - hours`, plus open tasks parked over
//! seven days with no `until`; each row carries its goal's rank, ages, touch
//! hours and flags, sorted by rank, then progress time, then id. [`run`] is
//! the command line: `--root` (else `$DASHBOARD_ROOT`, else the default
//! root), `--hours` (float), `--json`; exit 0, or 2 on a store error.
//! Known gap: an `--hours` that `timedelta` refuses (NaN, infinite, past
//! 999,999,999 days) and a list or dict `goal` make the script print a
//! traceback; the port prints its last line and exits 1.
//! Test: `stale_tasks_recordings_replay_clean_against_the_port`,
//! `shipped_stale_tasks_command_reads_the_working_directory_store`.

use std::path::PathBuf;

use crate::Clock;
use crate::argparse::{self, HELP, Opt, Parsed};
use crate::classify::alerts::{civil_from_days, days_from_civil};
use crate::dashboard::derive::now_micros;
use crate::dashboard::pending::from_iso_parts;
use crate::dashboard::store::Format;
use crate::poller::pyfmt::{py_float, py_float_repr};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::{py_repr, strip};
use crate::pyval::{cmp_int, dumps, get, has, is_str, py_str, truthy, type_name};
use crate::spec_drift::Outcome;

use super::store_read::{PyDoc, abspath, format_error, list_docs};

/// `derive.STALE_HOURS`.
pub const STALE_HOURS: f64 = 24.0;
const PARK_REVIEW_DAYS: i64 = 7;
const US_PER_HOUR: f64 = 3_600_000_000.0;

/// What the caller supplies besides argv and the clock.
#[derive(Debug, Clone)]
pub struct Options {
    /// The program name argparse prints.
    pub prog: String,
    /// The store when neither `--root` nor `$DASHBOARD_ROOT` names one.
    pub default_root: PathBuf,
    /// `$DASHBOARD_ROOT`.
    pub env_root: Option<String>,
    /// `$DASHBOARD_FORMAT`.
    pub dashboard_format: Option<String>,
}

/// An aware time: UTC and wall-clock microseconds.
#[derive(Debug, Clone, Copy)]
struct Time {
    utc: i64,
    local: i64,
}

/// `_ts(value)` of `dashboard_derive.py`.
fn ts(v: Option<&PyValue>) -> Option<Time> {
    let text = v
        .map_or_else(|| "None".to_owned(), py_str)
        .replace('Z', "+00:00");
    from_iso_parts(&text).map(|(local, offset)| Time {
        utc: local - offset,
        local,
    })
}

/// `max(times)` (the first of equal instants), else `_ts(created)`.
fn newest(cands: Vec<Option<&PyValue>>, d: &PyValue) -> Option<Time> {
    let mut best: Option<Time> = None;
    for t in cands.into_iter().filter_map(ts) {
        if best.is_none_or(|b| t.utc > b.utc) {
            best = Some(t);
        }
    }
    best.or_else(|| ts(get(d, "created")))
}

fn dict<'a>(d: &'a PyValue, k: &str) -> Option<&'a PyValue> {
    get(d, k).filter(|v| matches!(v, PyValue::Dict(_)))
}

fn ats<'a>(d: &'a PyValue, k: &str, out: &mut Vec<Option<&'a PyValue>>) {
    if let Some(PyValue::List(l)) = get(d, k) {
        out.extend(
            l.iter()
                .filter(|u| matches!(u, PyValue::Dict(_)))
                .map(|u| get(u, "at")),
        );
    }
}

/// `last_progress(data)`.
fn last_progress(d: &PyValue) -> Option<Time> {
    let mut c = vec![get(d, "progress_at")];
    ats(d, "log", &mut c);
    if let Some(rep) = dict(d, "reported") {
        c.push(get(rep, "at"));
    }
    if let Some(prog) = dict(d, "progress").filter(|_| !has(d, "progress_at")) {
        c.push(get(prog, "estimated_at"));
    }
    newest(c, d)
}

/// `last_activity(data)`.
fn last_activity(d: &PyValue) -> Option<Time> {
    let mut c = vec![get(d, "updated")];
    ats(d, "updates", &mut c);
    if let Some(prog) = dict(d, "progress") {
        c.push(get(prog, "estimated_at"));
    }
    newest(c, d)
}

fn open_task(d: &PyValue) -> bool {
    is_str(get(d, "kind"), "task") && is_str(get(d, "status"), "open")
}

/// `_flags(d, now)`.
fn flags(d: &PyValue, now: i64) -> Vec<String> {
    let prog = dict(d, "progress");
    let mut out = Vec::new();
    let pct = prog
        .and_then(|p| get(p, "pct"))
        .is_some_and(|v| !matches!(v, PyValue::Null));
    if !pct && !has(d, "steps") {
        out.push("no_progress".into());
    }
    let eta = prog
        .filter(|p| has(p, "eta"))
        .and_then(|p| ts(get(p, "eta")));
    if eta.is_some_and(|e| e.utc < now) {
        out.push("past_eta".into());
    }
    let session = get(d, "session")
        .filter(|v| truthy(v))
        .map_or(String::new(), py_str);
    if strip(&session).is_empty() {
        out.push("no_session".into());
    }
    out
}

/// One `stale_ranked` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    id: String,
    session: String,
    goal: Option<PyValue>,
    rank: Option<PyInt>,
    last: Option<String>,
    age_h: Option<f64>,
    days: Option<f64>,
    touch_h: Option<f64>,
    flags: Vec<String>,
    kind: &'static str,
    title: String,
}

/// `strftime("%Y-%m-%dT%H:%M:%SZ")` of a wall-clock instant.
fn strftime(local: i64) -> String {
    let secs = local.div_euclid(1_000_000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// `round(micros / 1e6 / per, 1)`.
fn round1(micros: i64, per: f64) -> f64 {
    let x = micros as f64 / 1e6 / per;
    format!("{x:.1}").parse().unwrap_or(x)
}

fn text(d: &PyValue, k: &str) -> String {
    get(d, k).map_or(String::new(), py_str)
}

/// `timedelta(hours=h)` in microseconds, or the last traceback line.
fn hours_us(h: f64) -> Result<i64, String> {
    if h.is_nan() {
        return Err("ValueError: cannot convert float NaN to integer".into());
    }
    if h.is_infinite() {
        return Err("OverflowError: cannot convert float infinity to integer".into());
    }
    let us = (h * US_PER_HOUR).round_ties_even();
    let days = (us / 86_400_000_000.0).floor();
    if days.abs() > 999_999_999.0 {
        return Err(format!(
            "OverflowError: days={days}; must have magnitude <= 999999999"
        ));
    }
    Ok(us as i64)
}

/// Why: the Architect's idle-task list (`stale_ranked`).
/// What: see the module doc; `now` is Unix seconds.
/// Test: `stale_tasks_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// The last traceback line where Python raises (see the module's known gap).
pub(crate) fn ranked(
    items: &[PyDoc],
    goals: &[PyDoc],
    now: f64,
    hours: f64,
) -> Result<Vec<Row>, String> {
    let now_us = now_micros(now);
    let min = days_from_civil(1, 1, 1) * 86_400_000_000;
    let cutoff = now_us - hours_us(hours)?;
    if cutoff < min || cutoff >= days_from_civil(10_000, 1, 1) * 86_400_000_000 {
        return Err("OverflowError: date value out of range".into());
    }
    let mut by_id: std::collections::HashMap<&str, Option<&PyInt>> =
        std::collections::HashMap::new();
    for g in goals {
        let r = match get(&g.data, "rank") {
            Some(PyValue::Int(i)) => Some(i),
            _ => None,
        };
        by_id.insert(&g.id, r);
    }
    let row = |doc: &PyDoc,
               last: Option<Time>,
               kind: &'static str,
               extra: &[&str]|
     -> Result<Row, String> {
        let d = &doc.data;
        let goal = get(d, "goal").filter(|v| truthy(v)).cloned();
        let rank = match &goal {
            Some(PyValue::Str(g)) => by_id.get(g.as_str()).copied().flatten().cloned(),
            Some(v @ (PyValue::List(_) | PyValue::Dict(_))) => {
                return Err(format!("TypeError: unhashable type: '{}'", type_name(v)));
            }
            _ => None,
        };
        let act = last_activity(d);
        let mut f = flags(d, now_us);
        f.extend(extra.iter().map(|s| (*s).to_owned()));
        Ok(Row {
            id: doc.id.clone(),
            session: text(d, "session"),
            goal,
            rank,
            last: last.map(|t| strftime(t.local)),
            age_h: last.map(|t| round1(now_us - t.utc, 3600.0)),
            days: last.map(|t| round1(now_us - t.utc, 86_400.0)),
            touch_h: act.map(|t| round1(now_us - t.utc, 3600.0)),
            flags: f,
            kind,
            title: text(d, "title"),
        })
    };
    let mut rows = Vec::new();
    for doc in items {
        let d = &doc.data;
        if !open_task(d) || has(d, "parked") || dict(d, "factory").is_some() {
            continue;
        }
        let last = last_progress(d);
        if last.is_none_or(|t| t.utc < cutoff) {
            rows.push(row(doc, last, "stale", &[])?);
        }
    }
    let park_cutoff = now_us - PARK_REVIEW_DAYS * 86_400_000_000;
    for doc in items {
        let Some(pk) = dict(&doc.data, "parked").filter(|_| open_task(&doc.data)) else {
            continue;
        };
        if has(pk, "until") {
            continue;
        }
        let at = ts(get(pk, "at"));
        if at.is_none_or(|t| t.utc < park_cutoff) {
            rows.push(row(doc, at, "parked", &["park_no_event"])?);
        }
    }
    rows.sort_by(|a, b| {
        let rank = match (&a.rank, &b.rank) {
            (Some(x), Some(y)) => cmp_int(x, y),
            (x, y) => x.is_none().cmp(&y.is_none()),
        };
        rank.then_with(|| a.last.is_some().cmp(&b.last.is_some()))
            .then_with(|| a.last.cmp(&b.last))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

fn opt_f(x: Option<f64>) -> PyValue {
    x.map_or(PyValue::Null, PyValue::Float)
}

impl Row {
    /// The row as the script's dict, keys in its order.
    fn to_py(&self) -> PyValue {
        let s = |x: &str| PyValue::Str(x.to_owned());
        PyValue::Dict(vec![
            ("id".into(), s(&self.id)),
            ("session".into(), s(&self.session)),
            ("goal".into(), self.goal.clone().unwrap_or(PyValue::Null)),
            (
                "rank".into(),
                self.rank.clone().map_or(PyValue::Null, PyValue::Int),
            ),
            ("last".into(), self.last.as_deref().map_or(PyValue::Null, s)),
            ("age_h".into(), opt_f(self.age_h)),
            ("days".into(), opt_f(self.days)),
            ("touch_h".into(), opt_f(self.touch_h)),
            (
                "flags".into(),
                PyValue::List(self.flags.iter().map(|f| s(f)).collect()),
            ),
            ("kind".into(), s(self.kind)),
            ("title".into(), s(&self.title)),
        ])
    }

    /// The tab-separated text line.
    fn line(&self) -> String {
        let flags = if self.flags.is_empty() {
            "-".to_owned()
        } else {
            self.flags.join(",")
        };
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.id,
            self.session,
            self.last.as_deref().unwrap_or("never"),
            self.goal.as_ref().map_or_else(|| "-".to_owned(), py_str),
            self.rank
                .as_ref()
                .map_or_else(|| "-".to_owned(), crate::pyval::int_text),
            self.title,
            self.days.map_or_else(|| "-".to_owned(), py_float_repr),
            self.kind,
            flags
        )
    }
}

const OPTS: [Opt; 4] = [
    HELP,
    Opt {
        long: "--root",
        short: None,
        value: true,
    },
    Opt {
        long: "--hours",
        short: None,
        value: true,
    },
    Opt {
        long: "--json",
        short: None,
        value: false,
    },
];

fn usage(prog: &str) -> String {
    argparse::usage(prog, "[-h] [--root ROOT] [--hours HOURS] [--json]")
}

fn help(prog: &str) -> String {
    format!(
        "{}\nstale-tasks.py: list open, unparked dashboard tasks (kind task) with no recent\nactivity.\n\n\
         options:\n  -h, --help     show this help message and exit\n  --root ROOT\n  --hours HOURS\n  \
         --json         print rows as JSON\n",
        usage(prog)
    )
}

fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// Why: the drop-in for the script's command line.
/// What: parses argv like argparse, reads the store and prints the rows
/// (tab-separated, or `json.dumps(rows, indent=1)` with `--json`).
/// Test: `stale_tasks_recordings_replay_clean_against_the_port`.
pub fn run(args: &[String], opts: &Options, clock: &dyn Clock) -> Outcome {
    let check = |i: usize, v: &str| match (i, py_float(v)) {
        (2, None) => Err(format!(
            "argument --hours: invalid float value: {}",
            py_repr(v)
        )),
        _ => Ok(()),
    };
    let a = match argparse::parse_checked(args, &OPTS, false, &check) {
        Parsed::Run(a) => a,
        Parsed::Help => return out(help(&opts.prog), String::new(), 0),
        Parsed::Error(msg) => {
            return out(
                String::new(),
                argparse::error(&usage(&opts.prog), &opts.prog, &msg),
                2,
            );
        }
    };
    let hours = a.values[2]
        .as_deref()
        .and_then(py_float)
        .unwrap_or(STALE_HOURS);
    let root = a.values[1]
        .clone()
        .or_else(|| opts.env_root.clone())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| opts.default_root.to_string_lossy().into_owned());
    let fail = |e: String| {
        out(
            String::new(),
            format!("stale-tasks: cannot read store: {e}\n"),
            2,
        )
    };
    if !std::path::Path::new(&root).is_dir() {
        return fail(format!("no dashboard store at {root}"));
    }
    let format = match opts.dashboard_format.as_deref() {
        None | Some("" | "dir") => Format::Dir,
        Some("jsonl") => Format::Jsonl,
        Some(v) => return fail(format_error(v)),
    };
    let abs = abspath(&root);
    let docs =
        list_docs(&abs, format, "items").and_then(|i| Ok((i, list_docs(&abs, format, "goals")?)));
    let (items, goals) = match docs {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let rows = match ranked(&items, &goals, clock.now_unix(), hours) {
        Ok(r) => r,
        Err(last_line) => return out(String::new(), format!("{last_line}\n"), 1),
    };
    if a.flags[3] {
        let list = PyValue::List(rows.iter().map(Row::to_py).collect());
        return out(
            format!("{}\n", dumps(&list, Some(1), true)),
            String::new(),
            0,
        );
    }
    out(rows.iter().map(Row::line).collect(), String::new(), 0)
}
