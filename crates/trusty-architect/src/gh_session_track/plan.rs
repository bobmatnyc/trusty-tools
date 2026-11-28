//! `build_plan()` of `gh-session-track.py`: PRs and issues mapped onto sessions (#12087).
//!
//! Why: the plan is what both the dry run prints and the write arm stores;
//! one function builds it for both.
//! What: [`build_plan`] maps every PR, issue and merged PR to one session
//! ([`map_session`]: a `ws/<x>` label, else a crate label of
//! [`SessionMap::crates`], else [`SessionMap::fallback`]), gives each PR one
//! chip state ([`pr_state`]), summarizes issues ([`summarize_issues`]), and
//! counts merges and closes inside 24 hours and 7 days of `now`. A session is
//! planned when its `project` contains [`SessionMap::mark`] or it has items.
//! Known gap: the script keys `per` by a `set`, so its `--json` session order
//! follows the string hash; the port orders sessions by name. A gh item whose
//! `labels`, `number` or `title` has a shape the script cannot use ends in a
//! traceback the port words as one line of its own.
//! Test: `gh_session_track_recordings_replay_clean_against_the_port`,
//! `map_session_prefers_ws_then_crate_then_fallback`, `pr_state_priority_order`.

use crate::pyjson_load::PyValue;
use crate::pystr::is_digit;
use crate::pystrptime::strptime;
use crate::pyval::{cmp_int, get, py_str, truthy, type_name};

/// `PR_CAP`.
pub(crate) const PR_CAP: i64 = 15;
/// `MERGED_DAYS`.
pub(crate) const MERGED_DAYS: i64 = 7;
/// `MERGED_LIMIT`.
pub(crate) const MERGED_LIMIT: usize = 1000;
const BUG_LABELS: [&str; 4] = ["type:bug", "bug", "bug:security", "bug:regression"];
const SECURITY_LABEL: &str = "bug:security";
const TOP_N: usize = 6;
const TITLE_MAX: usize = 80;
const BAD_CONCLUSIONS: [&str; 5] = [
    "FAILURE",
    "TIMED_OUT",
    "CANCELLED",
    "ERROR",
    "STARTUP_FAILURE",
];
const HOUR_US: i64 = 3_600_000_000;
const DAY_US: i64 = 24 * HOUR_US;

/// The instance data the script hard-codes (`CRATE_SESSIONS`,
/// `FALLBACK_SESSION`, `PROJECT_MARK`); the port takes it as input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionMap {
    /// Crate label to session name.
    pub crates: Vec<(String, String)>,
    /// The session an otherwise unmapped item lands on.
    pub fallback: Option<String>,
    /// Sessions whose `project` contains this are planned; empty plans none.
    pub mark: String,
}

/// One store session as the plan reads it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Session {
    pub(crate) name: String,
    pub(crate) id: String,
    pub(crate) version: u64,
    pub(crate) data: PyValue,
}

/// What the four gh reads returned.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fetched {
    pub(crate) prs: Vec<PyValue>,
    pub(crate) issues: Vec<PyValue>,
    pub(crate) merged: Vec<PyValue>,
    pub(crate) closed: Vec<PyValue>,
}

/// One PR chip: `{"n", "state", "url"}`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PrRow {
    pub(crate) n: PyValue,
    pub(crate) state: String,
    pub(crate) url: PyValue,
}

/// One top issue: `{"n", "title", "url", "tag"}`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Top {
    pub(crate) n: PyValue,
    pub(crate) title: String,
    pub(crate) url: PyValue,
    pub(crate) tag: &'static str,
}

/// `summarize_issues()`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Summary {
    pub(crate) open: i64,
    pub(crate) bugs: i64,
    pub(crate) bugs_p1: i64,
    pub(crate) p1: i64,
    pub(crate) p2: i64,
    pub(crate) security: i64,
    pub(crate) merged_open: i64,
    pub(crate) top: Vec<Top>,
}

/// `pr_stats`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PrStats {
    pub(crate) open: i64,
    pub(crate) by_state: Vec<(String, i64)>,
    pub(crate) merged_24h: i64,
    pub(crate) merged_7d: i64,
    pub(crate) capped: bool,
}

/// One planned session.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub(crate) prs: Vec<PrRow>,
    pub(crate) issues: Option<Summary>,
    pub(crate) pr_stats: PrStats,
}

/// `totals`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Totals {
    pub(crate) at: String,
    pub(crate) prs_open: i64,
    pub(crate) prs_by_state: Vec<(String, i64)>,
    pub(crate) merged_24h: i64,
    pub(crate) merged_7d: i64,
    pub(crate) capped: bool,
    pub(crate) issues_open: i64,
    pub(crate) all: Summary,
    pub(crate) closed_24h: i64,
    pub(crate) closed_7d: i64,
    pub(crate) awaiting_release: i64,
}

/// The plan: sessions by name, then the repo totals.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub(crate) sessions: Vec<(String, Entry)>,
    pub(crate) totals: Totals,
}

/// `parse_ts(s)`: microseconds since the epoch, `None` where `strptime` raises.
pub(crate) fn parse_ts(v: &PyValue) -> Option<i64> {
    strptime(&py_str(v), "%Y-%m-%dT%H:%M:%SZ").map(|(us, _)| us)
}

fn bump(counts: &mut Vec<(String, i64)>, key: &str) {
    match counts.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 += 1,
        None => counts.push((key.to_owned(), 1)),
    }
}

/// `item["number"]`, or the last line of the traceback it raises.
pub(crate) fn number(item: &PyValue) -> Result<PyValue, String> {
    match item {
        PyValue::Dict(_) => get(item, "number")
            .cloned()
            .ok_or_else(|| "KeyError: 'number'".to_owned()),
        PyValue::List(_) => {
            Err("TypeError: list indices must be integers or slices, not str".into())
        }
        PyValue::Str(_) => Err("TypeError: string indices must be integers, not 'str'".into()),
        other => Err(format!(
            "TypeError: '{}' object is not subscriptable",
            type_name(other)
        )),
    }
}

/// `x or ""`.
fn or_empty(v: Option<&PyValue>) -> PyValue {
    v.filter(|v| truthy(v))
        .cloned()
        .unwrap_or_else(|| PyValue::Str(String::new()))
}

/// `label_names(item)`.
fn label_names(item: &PyValue) -> Vec<String> {
    let Some(PyValue::List(labels)) = get(item, "labels") else {
        return Vec::new();
    };
    labels
        .iter()
        .map(|x| match x {
            PyValue::Dict(_) => get(x, "name")
                .filter(|v| truthy(v))
                .map(py_str)
                .unwrap_or_default(),
            other => py_str(other),
        })
        .collect()
}

/// `resolve_ws(x, names)`: the exact name, else the unique `<x>-NN` session.
fn resolve_ws(x: &str, names: &[String]) -> Option<String> {
    if names.iter().any(|n| n == x) {
        return Some(x.to_owned());
    }
    let prefix = format!("{x}-");
    let cands: Vec<&String> = names
        .iter()
        .filter(|n| {
            n.strip_prefix(&prefix)
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(is_digit))
        })
        .collect();
    match cands.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// `map_session(item, names)`: one session name, or `None`.
///
/// Why: every PR, issue and merged PR lands on at most one session.
/// What: a `ws/<x>` label that [`resolve_ws`] finds, else the first crate
/// label whose session exists, else the fallback when it exists.
/// Test: `map_session_prefers_ws_then_crate_then_fallback`.
pub(crate) fn map_session(item: &PyValue, names: &[String], map: &SessionMap) -> Option<String> {
    let labels = label_names(item);
    for lb in &labels {
        if let Some(hit) = lb.strip_prefix("ws/").and_then(|x| resolve_ws(x, names)) {
            return Some(hit);
        }
    }
    for lb in &labels {
        let hit = map.crates.iter().find(|(c, _)| c == lb).map(|(_, s)| s);
        if let Some(hit) = hit.filter(|s| names.contains(s)) {
            return Some(hit.clone());
        }
    }
    map.fallback.clone().filter(|f| names.contains(f))
}

fn upper(v: Option<&PyValue>) -> String {
    v.filter(|v| truthy(v))
        .map(py_str)
        .unwrap_or_default()
        .to_uppercase()
}

/// `pr_state(pr)`: draft, red, running, dirty, hold, ready, else the merge state.
///
/// Why: one state word per PR chip.
/// What: the script's priority order over `isDraft`, `statusCheckRollup`
/// (conclusion, status or the status-context `state`), `mergeStateStatus`
/// and the `do-not-merge` label.
/// Test: `pr_state_priority_order`.
pub(crate) fn pr_state(pr: &PyValue) -> String {
    if get(pr, "isDraft").is_some_and(truthy) {
        return "draft".into();
    }
    let checks: &[PyValue] = match get(pr, "statusCheckRollup") {
        Some(PyValue::List(c)) => c,
        _ => &[],
    };
    let red = checks.iter().any(|c| {
        BAD_CONCLUSIONS.contains(&upper(get(c, "conclusion")).as_str())
            || matches!(upper(get(c, "state")).as_str(), "FAILURE" | "ERROR")
    });
    if red {
        return "red".into();
    }
    for c in checks {
        let has = |k: &str| get(c, k).is_some();
        if has("status") && upper(get(c, "status")) != "COMPLETED" {
            return "running".into();
        }
        if has("state")
            && !has("status")
            && matches!(upper(get(c, "state")).as_str(), "PENDING" | "EXPECTED")
        {
            return "running".into();
        }
    }
    let ms = upper(get(pr, "mergeStateStatus"));
    if ms == "DIRTY" {
        return "dirty".into();
    }
    if label_names(pr).iter().any(|l| l == "do-not-merge") {
        return "hold".into();
    }
    if ms == "CLEAN" {
        return "ready".into();
    }
    match ms.to_lowercase() {
        s if s.is_empty() => "unknown".into(),
        s => s,
    }
}

/// `issue_flags(issue)`: `(bug, security, p1, p2, merged)`.
fn issue_flags(issue: &PyValue) -> (bool, bool, bool, bool, bool) {
    let labels = label_names(issue);
    let has = |l: &str| labels.iter().any(|x| x == l);
    (
        BUG_LABELS.iter().any(|b| has(b)),
        has(SECURITY_LABEL),
        has("P1"),
        has("P2"),
        has("status:merged"),
    )
}

fn sort_number(a: &PyValue, b: &PyValue) -> std::cmp::Ordering {
    match (a, b) {
        (PyValue::Int(x), PyValue::Int(y)) => cmp_int(x, y),
        _ => std::cmp::Ordering::Equal,
    }
}

/// `summarize_issues(issues)`: counts plus up to six top issues.
pub(crate) fn summarize_issues(issues: &[&PyValue]) -> Result<Summary, String> {
    let mut out = Summary {
        open: issues.len() as i64,
        ..Summary::default()
    };
    let mut ranked: Vec<(u8, PyValue, &'static str, &PyValue)> = Vec::new();
    for i in issues {
        let (bug, sec, p1, p2, merged) = issue_flags(i);
        out.bugs += i64::from(bug);
        out.bugs_p1 += i64::from(bug && p1);
        out.p1 += i64::from(p1);
        out.p2 += i64::from(p2);
        out.security += i64::from(sec);
        out.merged_open += i64::from(merged);
        let (tier, tag) = match (sec, p1, p2, bug) {
            (true, ..) => (0, "security"),
            (_, true, _, true) => (1, "P1 bug"),
            (_, true, ..) => (2, "P1"),
            (_, _, true, true) => (3, "P2 bug"),
            _ => continue,
        };
        ranked.push((tier, number(i)?, tag, i));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| sort_number(&a.1, &b.1)));
    out.top = ranked
        .into_iter()
        .take(TOP_N)
        .map(|(_, n, tag, i)| Top {
            n,
            title: match get(i, "title").filter(|v| truthy(v)) {
                Some(t) => py_str(t).chars().take(TITLE_MAX).collect(),
                None => String::new(),
            },
            url: or_empty(get(i, "url")),
            tag,
        })
        .collect();
    Ok(out)
}

/// `merged_windows(merged, now)`: merged PRs inside 7 days, with "inside 24 hours".
fn merged_windows(merged: &[PyValue], now: i64) -> Vec<(&PyValue, bool)> {
    merged
        .iter()
        .filter_map(|m| {
            let at = parse_ts(get(m, "mergedAt").unwrap_or(&PyValue::Null))?;
            (at <= now && now - at <= MERGED_DAYS * DAY_US).then_some((m, now - at <= DAY_US))
        })
        .collect()
}

/// `closed_counts(closed, now)`: closed in 24 hours and in 7 days.
fn closed_counts(closed: &[PyValue], now: i64) -> (i64, i64) {
    let ages: Vec<i64> = closed
        .iter()
        .filter_map(parse_ts)
        .filter(|a| *a <= now)
        .map(|a| now - a)
        .filter(|x| *x <= MERGED_DAYS * DAY_US)
        .collect();
    let day = ages.iter().filter(|x| **x <= DAY_US).count();
    (day as i64, ages.len() as i64)
}

/// `awaiting_release(issues, merged)`.
fn awaiting_release(issues: &[PyValue], merged: &[PyValue]) -> Result<i64, String> {
    let mut refs: Vec<PyValue> = Vec::new();
    for m in merged {
        if let Some(PyValue::List(rs)) = get(m, "closingIssuesReferences") {
            refs.extend(
                rs.iter()
                    .map(|r| get(r, "number").cloned().unwrap_or(PyValue::Null)),
            );
        }
    }
    let mut n = 0;
    for i in issues {
        if issue_flags(i).4 || refs.contains(&number(i)?) {
            n += 1;
        }
    }
    Ok(n)
}

/// `build_plan(prs, issues, sessions, now, merged, closed)`.
///
/// Why: see the module doc.
/// What: `now` is the run's `%Y-%m-%dT%H:%M:%SZ` instant; `sessions` holds
/// each name once. Errors are the last line of the script's traceback.
/// Test: `gh_session_track_recordings_replay_clean_against_the_port`.
pub(crate) fn build_plan(
    f: &Fetched,
    sessions: &[Session],
    now: &str,
    map: &SessionMap,
) -> Result<Plan, String> {
    let t = parse_ts(&PyValue::Str(now.to_owned())).ok_or("ValueError: bad now")?;
    let mut names: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();
    names.sort();
    let mut per: Vec<(Vec<PrRow>, Vec<&PyValue>, i64, i64)> = names
        .iter()
        .map(|_| (Vec::new(), Vec::new(), 0, 0))
        .collect();
    let slot = |s: &str| names.iter().position(|n| n == s);
    let mut prs: Vec<(PyValue, &PyValue)> = f
        .prs
        .iter()
        .map(|p| Ok((number(p)?, p)))
        .collect::<Result<_, String>>()?;
    prs.sort_by(|a, b| sort_number(&a.0, &b.0));
    let mut by_state = Vec::new();
    for (n, pr) in prs {
        let st = pr_state(pr);
        bump(&mut by_state, &st);
        if let Some(i) = map_session(pr, &names, map).and_then(|s| slot(&s)) {
            let url = or_empty(get(pr, "url"));
            per[i].0.push(PrRow { n, state: st, url });
        }
    }
    for issue in &f.issues {
        if let Some(i) = map_session(issue, &names, map).and_then(|s| slot(&s)) {
            per[i].1.push(issue);
        }
    }
    let win = merged_windows(&f.merged, t);
    let capped = f.merged.len() >= MERGED_LIMIT;
    for (m, in24) in &win {
        if let Some(i) = map_session(m, &names, map).and_then(|s| slot(&s)) {
            per[i].3 += 1;
            per[i].2 += i64::from(*in24);
        }
    }
    let mut plan = Vec::new();
    for (name, (rows, issues, m24, m7)) in names.iter().zip(per) {
        let data = sessions
            .iter()
            .rev()
            .find(|s| &s.name == name)
            .map(|s| &s.data);
        let project = data
            .and_then(|d| get(d, "project"))
            .filter(|v| truthy(v))
            .map(py_str);
        let tracked = !map.mark.is_empty() && project.unwrap_or_default().contains(&map.mark);
        if !(tracked || !rows.is_empty() || !issues.is_empty()) {
            continue;
        }
        let mut st = Vec::new();
        for r in &rows {
            bump(&mut st, &r.state);
        }
        let pr_stats = PrStats {
            open: rows.len() as i64,
            by_state: st,
            merged_24h: m24,
            merged_7d: m7,
            capped,
        };
        let summary = if issues.is_empty() {
            None
        } else {
            Some(summarize_issues(&issues)?)
        };
        plan.push((
            name.clone(),
            Entry {
                prs: rows,
                issues: summary,
                pr_stats,
            },
        ));
    }
    let all: Vec<&PyValue> = f.issues.iter().collect();
    let (closed_24h, closed_7d) = closed_counts(&f.closed, t);
    let totals = Totals {
        at: now.to_owned(),
        prs_open: f.prs.len() as i64,
        prs_by_state: by_state,
        merged_24h: win.iter().filter(|(_, d)| *d).count() as i64,
        merged_7d: win.len() as i64,
        capped,
        issues_open: f.issues.len() as i64,
        all: summarize_issues(&all)?,
        closed_24h,
        closed_7d,
        awaiting_release: awaiting_release(&f.issues, &f.merged)?,
    };
    Ok(Plan {
        sessions: plan,
        totals,
    })
}
