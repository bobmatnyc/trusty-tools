//! `dashboard-db.py`'s read-only reports: tally, footer, summary, requests, validate, fsck, pruned (#12029 P2g).
//!
//! Why: replies quote the footer and the resume prompt reads the summary;
//! both come from the one tally path, and an unreadable store is an error,
//! never a guessed count.
//! What: [`tally`] (`dashboard_derive.tally` plus `running` and, when non-zero,
//! `answers_waiting`), [`tally_line`], [`footer`] (the links line, the
//! questions part, the reminders line), [`summary_lines`], [`requests`],
//! [`validate`], [`fsck`] and [`pruned_lines`].
//! Test: `dashboard_db_recordings_replay_clean_against_the_port` (`tally-*`,
//! `footer-*`, `summary`, `requests-*`, `validate`, `fsck-*`, `pruned-*`).

use std::path::Path;

use super::checks::{
    has_goal_projects, open_running_task, task_link_warnings, untracked_open_tasks,
};
use super::{Ctx, Fail, R, dict, int, refuse, s};
use crate::dashboard::pyanswers;
use crate::dashboard::pyderive::{counts, pending};
use crate::dashboard::pyjsonl;
use crate::dashboard::pyqual;
use crate::dashboard::store::py::{Fmt, PyStoreError};
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::collapse_space;
use crate::pyval::{get, is_str, py_str, truthy, type_name};
use crate::tasks::store_read::PyDoc;

/// `LOCAL_URL`.
const LOCAL_URL: &str = "http://127.0.0.1:7890/";
/// `SUMMARY_MAX_ANSWERS`.
const SUMMARY_MAX_ANSWERS: usize = 18;
/// What `requests` snapshots.
const REQUEST_COLLECTIONS: [&str; 6] = [
    "items",
    "sessions",
    "effort_tunes",
    "il_steers",
    "goals",
    "instructions",
];

fn status_is(d: &PyDoc, st: &str) -> bool {
    is_str(get(&d.data, "status"), st)
}

/// Why: the only source for dashboard counts quoted in replies.
/// What: `tally(store)`: derive's counts over `items`, `running` at now, and
/// `answers_waiting` (open `questions` docs) only when non-zero.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`tally-json`).
pub(crate) fn tally(ctx: &Ctx) -> Result<PyValue, PyStoreError> {
    let docs = ctx.docs("items")?;
    let PyValue::Dict(mut out) = counts::tally(&docs) else {
        return Ok(PyValue::Null);
    };
    out.push((
        "running".into(),
        int(counts::running_count(&docs, ctx.now())),
    ));
    let waiting = ctx
        .docs("questions")?
        .iter()
        .filter(|d| status_is(d, "open"))
        .count();
    if waiting > 0 {
        out.push(("answers_waiting".into(), int(waiting as i64)));
    }
    Ok(PyValue::Dict(out))
}

/// `tally_line(t)`: `"<d> decisions, <b> blockers"` (#12029 P2g).
pub(crate) fn tally_line(t: &PyValue) -> String {
    let n = |k: &str| get(t, k).map_or("None".into(), py_str);
    format!("{} decisions, {} blockers", n("decisions"), n("blockers"))
}

/// `open(path)` + `json.load`: `None` on an `OSError` or a `ValueError`.
fn read_json(path: &Path) -> Option<PyValue> {
    let text = crate::pyjson_file::read_text(path).ok()?;
    loads(&text).ok()
}

/// `st.get(k)` on the questions state, which the script reads as a dict.
fn state_get<'a>(st: &'a PyValue, k: &str) -> R<Option<&'a PyValue>> {
    match st {
        PyValue::Dict(_) => Ok(get(st, k)),
        other => Err(Fail::Raise(format!(
            "AttributeError: '{}' object has no attribute 'get'",
            type_name(other)
        ))),
    }
}

/// Why: the footer links the questions session only while it has a live URL.
/// What: `questions_part(n)`: `""` for 0, else `questions: N`, linked to the
/// `url` of `$QUESTIONS_RC_STATE` (else `<checkout>/inbox/questions-rc.json`)
/// when its `rc` is `"on"`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`footer-linked`, `footer-unlinked`).
pub(crate) fn questions_part(ctx: &Ctx, n: &PyValue) -> R<String> {
    let n = match n {
        PyValue::Int(crate::pyjson_load::PyInt::Small(k)) => usize::try_from(*k).unwrap_or(0),
        _ => 0,
    };
    if n == 0 {
        return Ok(String::new());
    }
    let path = (ctx.o.env)("QUESTIONS_RC_STATE")
        .filter(|p| !p.is_empty())
        .map_or_else(
            || ctx.o.checkout.join("inbox").join("questions-rc.json"),
            |p| ctx.o.cwd.join(p),
        );
    let st = read_json(&path).unwrap_or(PyValue::Dict(Vec::new()));
    let on = state_get(&st, "rc")?.is_some_and(|v| is_str(Some(v), "on"));
    let url = if on {
        state_get(&st, "url")?.filter(|u| truthy(u)).map(py_str)
    } else {
        None
    };
    Ok(crate::dashboard::tally::questions_part(n, url.as_deref()))
}

/// Why: replies end with one footer from the one tally path.
/// What: `footer(store)`: `{"footer", "tally", "reminders"}`; a root that is
/// not a directory, or an `OSError` while counting, is a store error.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`footer-*`).
pub(crate) fn footer(ctx: &Ctx) -> R<PyValue> {
    if !Path::new(&ctx.store.root).is_dir() {
        return refuse(format!("store root not readable: {}", ctx.store.root));
    }
    let t = match tally(ctx) {
        Ok(t) => t,
        Err(e @ PyStoreError::Os { .. }) => return refuse(format!("store not readable: {e}")),
        Err(e) => return Err(e.into()),
    };
    let tail = if ctx.tail_host.is_empty() {
        String::new()
    } else {
        format!(" \u{b7} [tail]({})", ctx.tail_host)
    };
    let mut line = format!(
        "Dashboard: [local]({LOCAL_URL}){tail} \u{2014} {}",
        tally_line(&t)
    );
    let q = questions_part(ctx, get(&t, "interactive").unwrap_or(&int(0)))?;
    if !q.is_empty() {
        line.push_str(" \u{b7} ");
        line.push_str(&q);
    }
    let reminders = super::reminders::reminders_line(ctx);
    Ok(dict(vec![
        ("footer", s(&line)),
        ("tally", t),
        ("reminders", s(&reminders)),
    ]))
}

/// `for t in thread or []`: each entry has an answer; a shape the script
/// cannot walk is its traceback.
fn waiting_follow_up(thread: Option<&PyValue>) -> R<bool> {
    let entries = match thread.filter(|t| truthy(t)) {
        None => return Ok(false),
        Some(PyValue::List(l)) => l,
        Some(PyValue::Str(_) | PyValue::Dict(_)) => {
            return Err(Fail::Raise(
                "AttributeError: 'str' object has no attribute 'get'".into(),
            ));
        }
        Some(v) => {
            return Err(Fail::Raise(format!(
                "TypeError: '{}' object is not iterable",
                type_name(v)
            )));
        }
    };
    for t in entries {
        match t {
            v if !truthy(v) => return Ok(true),
            PyValue::Dict(_) if !get(t, "answer").is_some_and(truthy) => return Ok(true),
            PyValue::Dict(_) => {}
            v => {
                return Err(Fail::Raise(format!(
                    "AttributeError: '{}' object has no attribute 'get'",
                    type_name(v)
                )));
            }
        }
    }
    Ok(false)
}

/// Why: the resume prompt reads at most ~25 lines instead of `list items`.
/// What: `summary_lines(store)`: user state, open counts, the interactive
/// ids, the answers to relay (cut to 60 characters), open questions, the
/// follow-ups and the decision counts.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`summary`).
pub(crate) fn summary_lines(ctx: &Ctx) -> R<Vec<String>> {
    let settings = match ctx.get("meta", &s("settings")) {
        Ok(d) => d.data,
        Err(PyStoreError::NotFound(_)) => PyValue::Dict(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let or = |k: &str, d: &str| get(&settings, k).map_or(d.to_owned(), py_str);
    let mut out = vec![format!(
        "user_state: {} since {}",
        or("user_state", "active"),
        or("user_state_at", "unknown")
    )];
    let docs = ctx.docs("items")?;
    let t = counts::tally(&docs);
    let n = |k: &str| get(&t, k).map_or(String::new(), py_str);
    out.push(format!(
        "open: {} (D {}, A {}, T {})",
        n("open"),
        n("questions"),
        n("actions"),
        n("tasks")
    ));
    let asked: Vec<&str> = docs
        .iter()
        .filter(|d| status_is(d, "interactive"))
        .map(|d| d.id.as_str())
        .collect();
    if !asked.is_empty() {
        out.push(format!(
            "asked interactively (open until answered): {}",
            asked.join(", ")
        ));
    }
    let (to_decide, waiting) = counts::decision_counts(&docs);
    let trial = (ctx.o.env)("DASHBOARD_ACTOR").as_deref() == Some(pyanswers::ILIO_ACTOR);
    let fresh = crate::dashboard::store::py::PyStore::new(
        &ctx.store.root,
        Default::default(),
        ctx.o.env,
        &ctx.o.cwd,
        ctx.schema_dir(),
        ctx.o.clock.clone(),
    )?;
    let pend = pyanswers::pending(&fresh, ctx.now(), trial)?;
    let sid = pyqual::store_sid(&ctx.store, &ctx.default_root())?;
    out.push(format!("answers to relay: {}", pend.len()));
    for e in pend.iter().take(SUMMARY_MAX_ANSWERS) {
        let text = collapse_space(&get(e, "text").map_or(String::new(), py_str));
        let text = if text.chars().count() <= 60 {
            text
        } else {
            format!("{}\u{2026}", text.chars().take(59).collect::<String>())
        };
        let id = get(e, "id").map_or(String::new(), py_str);
        let sess = docs
            .iter()
            .find(|d| d.id == id)
            .and_then(|d| get(&d.data, "session"))
            .filter(|v| truthy(v))
            .map_or("-".to_owned(), py_str);
        let kind = get(e, "kind").map_or(String::new(), py_str);
        let at = get(e, "at").map_or(String::new(), py_str);
        out.push(format!(
            "  {} {kind} {sess} {at} {text}",
            pyqual::qualify(sid.as_deref(), &id)
        ));
    }
    if pend.len() > SUMMARY_MAX_ANSWERS {
        out.push(format!(
            "  ... {} more (python3 scripts/pending-answers.py)",
            pend.len() - SUMMARY_MAX_ANSWERS
        ));
    }
    let questions = ctx.docs("questions")?;
    let qopen = questions
        .iter()
        .filter(|d| {
            !status_is(d, "cleared")
                && !status_is(d, "pending")
                && !get(&d.data, "cleared_at").is_some_and(truthy)
        })
        .count();
    out.push(format!("questions open: {qopen}"));
    let mut fu = Vec::new();
    for d in questions.iter().filter(|d| status_is(d, "pending")) {
        if waiting_follow_up(get(&d.data, "thread"))? {
            fu.push(d.id.as_str());
        }
    }
    if !fu.is_empty() {
        out.push(format!(
            "question follow-ups waiting: {} ({})",
            fu.len(),
            fu.join(", ")
        ));
    }
    out.push(format!(
        "decisions: {to_decide} to decide, {waiting} answered, waiting for the Architect"
    ));
    Ok(out)
}

/// `derive.request_queue(snapshot, now)`: the owner requests waiting, oldest first.
pub(crate) fn requests(ctx: &Ctx) -> R<Vec<PyValue>> {
    Ok(pending::request_queue(
        &ctx.snapshot(&REQUEST_COLLECTIONS)?,
        ctx.now(),
    ))
}

/// The `requests` text line: `at`, `id`, `label`, and `text` cut to 80 characters.
pub(crate) fn request_line(e: &PyValue) -> String {
    let f = |k: &str| get(e, k).map_or(String::new(), py_str);
    let text: String = f("text").chars().take(80).collect();
    format!("{}\t{}\t{}\t{text}\n", f("at"), f("id"), f("label"))
}

/// `dsch.validate_store(snap)`: every document, then the goal and step cross references.
fn validate_store(ctx: &Ctx, snap: &[(String, Vec<PyDoc>)]) -> R<Vec<String>> {
    let mut errs = Vec::new();
    for (coll, docs) in snap {
        for d in docs {
            errs.extend(super::checks::validate(ctx, coll, &d.id, &d.data)?);
        }
    }
    let of = |c: &str| snap.iter().find(|(k, _)| k == c).map(|(_, v)| v.as_slice());
    let ids = |c: &str| {
        of(c)
            .unwrap_or(&[])
            .iter()
            .map(|d| d.id.clone())
            .collect::<Vec<_>>()
    };
    let field = |d: &PyDoc, k: &str| get(&d.data, k).cloned().unwrap_or(PyValue::Null);
    if of("goals").is_some() {
        let goals = ids("goals");
        for d in of("items").unwrap_or(&[]) {
            if let PyValue::Str(g) = field(d, "goal")
                && !goals.contains(&g)
            {
                errs.push(format!(
                    "items/{}: /goal: {g} is not a goal id in the store",
                    d.id
                ));
            }
        }
        for d in of("goal_removals").unwrap_or(&[]) {
            let g = field(d, "goal");
            if !matches!(&g, PyValue::Str(x) if goals.contains(x)) {
                errs.push(format!(
                    "goal_removals/{}: /goal: {} is not a goal id in the store",
                    d.id,
                    py_str(&g)
                ));
            }
        }
    }
    if of("items").is_some() {
        let items = ids("items");
        for d in of("step_progress").unwrap_or(&[]) {
            let i = field(d, "item");
            if !matches!(&i, PyValue::Str(x) if items.contains(x)) {
                errs.push(format!(
                    "step_progress/{}: /item: {} is not an item id in the store",
                    d.id,
                    py_str(&i)
                ));
            }
        }
    }
    Ok(errs)
}

/// Why: `validate` checks the whole store against the schemas and the task rules.
/// What: `{errors, warnings, warning_counts, documents}`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`validate`).
pub(crate) fn validate(ctx: &Ctx) -> R<PyValue> {
    let mut snap = Vec::new();
    for c in ctx.store.collections()? {
        let docs = ctx.docs(&c)?;
        snap.push((c, docs));
    }
    snap.sort_by(|a, b| a.0.cmp(&b.0));
    let items: Vec<PyDoc> = snap
        .iter()
        .find(|(k, _)| k == "items")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let mut errs = validate_store(ctx, &snap)?;
    errs.extend(
        untracked_open_tasks(&items)
            .into_iter()
            .map(|i| format!("items/{i}: open task has no steps and no progress")),
    );
    errs.extend(
        items
            .iter()
            .filter(|d| open_running_task(&d.data) && !has_goal_projects(&d.data))
            .map(|d| format!("items/{}: open task has no goal or no projects", d.id)),
    );
    let (none, noticket) = task_link_warnings(&items);
    let mut warns: Vec<PyValue> = none
        .iter()
        .map(|i| {
            s(&format!(
                "items/{i}: task has no link (ticket, PR, spec or brief)"
            ))
        })
        .collect();
    warns.extend(noticket.iter().map(|i| {
        s(&format!(
            "items/{i}: delegated task has no GitHub issue or PR link"
        ))
    }));
    let documents: usize = snap.iter().map(|(_, v)| v.len()).sum();
    Ok(dict(vec![
        ("errors", PyValue::List(errs.iter().map(|e| s(e)).collect())),
        ("warnings", PyValue::List(warns)),
        (
            "warning_counts",
            dict(vec![
                ("no_link", int(none.len() as i64)),
                ("no_github_ticket", int(noticket.len() as i64)),
            ]),
        ),
        ("documents", int(documents as i64)),
    ]))
}

/// `fsck`: the jsonl store's problems; a dir store is refused.
pub(crate) fn fsck(ctx: &Ctx) -> R<PyValue> {
    if ctx.store.fmt != Fmt::Jsonl {
        return refuse("fsck needs DASHBOARD_FORMAT=jsonl");
    }
    let problems = pyjsonl::fsck(&ctx.store.root).map_err(|e| match e {
        pyjsonl::JsonlError::Bad(m) => Fail::Raise(format!("dashboard_jsonl.JsonlError: {m}")),
        pyjsonl::JsonlError::Os(m) => Fail::Raise(format!("OSError: {m}")),
    })?;
    Ok(dict(vec![(
        "problems",
        PyValue::List(problems.iter().map(|p| s(p)).collect()),
    )]))
}

/// Why: the prune log is the only record of a deleted document.
/// What: `pruned_lines(store, id, collection)`: the raw lines matching both
/// filters, oldest first; no log is no lines; a line that is not a JSON
/// object is the script's traceback.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`pruned-*`).
pub(crate) fn pruned_lines(ctx: &Ctx, id: Option<&str>, coll: Option<&str>) -> R<Vec<String>> {
    let path = ctx.store.prune_log_path();
    let text = match crate::pyjson_file::read_text(Path::new(&path)) {
        Ok(t) => t,
        Err(crate::pyjson_file::LoadError::NotFound(_)) => return Ok(Vec::new()),
        Err(crate::pyjson_file::LoadError::Other(m)) => {
            return Err(Fail::Raise(format!("OSError: {m}")));
        }
    };
    let mut out = Vec::new();
    for line in crate::pystr::splitlines(&text) {
        let rec = loads(line)
            .map_err(|e| Fail::Raise(format!("json.decoder.JSONDecodeError: {}", e.py_str())))?;
        let keep = |k: &str, want: Option<&str>| want.is_none_or(|w| is_str(get(&rec, k), w));
        if !matches!(rec, PyValue::Dict(_)) {
            return Err(Fail::Raise(format!(
                "AttributeError: '{}' object has no attribute 'get'",
                type_name(&rec)
            )));
        }
        if keep("id", id) && keep("collection", coll) {
            out.push(line.to_owned());
        }
    }
    Ok(out)
}

/// `fsck`/`validate`'s exit: 1 when `problems` or `errors` is non-empty.
pub(crate) fn report_exit(out: &PyValue) -> i32 {
    let some = |k: &str| get(out, k).is_some_and(truthy);
    i32::from(some("problems") || some("errors"))
}
