//! `dashboard-db.py`'s write refusals and the `validate` task checks (#12029 P2g).
//!
//! Why: the store accepts any JSON object; these rules keep an item the page
//! cannot render, an open task with no progress, goal or projects, an
//! invalid question, settings level or site block, a second recommended
//! option and a blind overwrite out of the store, with the frozen script's
//! words. An `update` is checked as the merged document.
//! What: [`check_steps`], [`check_task_progress`], [`check_task_goal_projects`],
//! [`check_question`], [`check_settings_level`], [`check_site`],
//! [`check_recommended`] (sets the one flag a "(Recommended)" label names),
//! [`check_prefix_open`], [`check_set_new`], [`check_ops`] (a batch), and the
//! `validate` readers [`untracked_open_tasks`], [`has_goal_projects`] and
//! [`task_link_warnings`].
//! Test: `dashboard_db_recordings_replay_clean_against_the_port` (`refusals`,
//! `batch-*`), `every_write_refusal_leaves_the_store_unchanged`.

use super::{Ctx, Fail, R, dict, has_key, id_digit, merge, refuse, s, set};
use crate::dashboard::pyqual;
use crate::dashboard::store::py::PyStoreError;
use crate::dashboard::store::py::to_json;
use crate::pyjson_load::PyValue;
use crate::pystr::{py_repr, strip};
use crate::pyval::{get, is_str, py_str, repr};
use crate::tasks::store_read::PyDoc;

/// `STEP_FIELDS`.
const STEP_FIELDS: &str = "id, title, detail, links, values";
/// `OPEN_TASK_STATUSES`.
const OPEN_TASK_STATUSES: [&str; 6] = ["open", "running", "hold", "parked", "blocked", "waiting"];
/// `OWN_PROJECTS`.
const OWN_PROJECTS: [&str; 2] = ["architect", "bobmatnyc/architect"];

/// `str(v)` of an id (`None` for a missing one).
fn id_text(id: &PyValue) -> String {
    py_str(id)
}

/// `check_steps(item_id, data)`: `steps` is a list of objects with a non-empty `title`.
pub(crate) fn check_steps(id: &PyValue, data: &PyValue) -> R<()> {
    let Some(steps) = get(data, "steps") else {
        return Ok(());
    };
    let id = id_text(id);
    let PyValue::List(steps) = steps else {
        return refuse(format!(
            "item {id}: steps must be a list of objects with fields: {STEP_FIELDS}"
        ));
    };
    for (i, st) in steps.iter().enumerate() {
        let at = format!("item {id}: step {i}");
        if !matches!(st, PyValue::Dict(_)) {
            return refuse(format!("{at} must be an object with fields: {STEP_FIELDS}"));
        }
        if !matches!(get(st, "title"), Some(PyValue::Str(t)) if !strip(t).is_empty()) {
            let hint = if has_key(st, "text") {
                " ('text' is not rendered; rename it to 'title')"
            } else {
                ""
            };
            return refuse(format!(
                "{at} needs a non-empty string 'title'{hint}; expected fields: {STEP_FIELDS}"
            ));
        }
    }
    Ok(())
}

/// `needs_progress(doc_id, data)`: an open task, or a T-prefixed action.
fn needs_progress(id: &PyValue, d: &PyValue) -> bool {
    let t_action = is_str(get(d, "kind"), "action") && super::text_or(Some(id)).starts_with('T');
    (is_str(get(d, "kind"), "task") || t_action)
        && OPEN_TASK_STATUSES
            .iter()
            .any(|st| is_str(get(d, "status"), st))
}

/// `has_progress(data)`: a numeric `progress` or `progress.pct` (not a bool).
fn has_progress(d: &PyValue) -> bool {
    let p = match get(d, "progress") {
        Some(p @ PyValue::Dict(_)) => get(p, "pct"),
        other => other,
    };
    matches!(p, Some(PyValue::Int(_) | PyValue::Float(_)))
}

/// `untracked_open_tasks(docs)`: open tasks with no progress, sorted.
pub(crate) fn untracked_open_tasks(docs: &[PyDoc]) -> Vec<String> {
    let mut out: Vec<String> = docs
        .iter()
        .filter(|d| matches!(d.data, PyValue::Dict(_)))
        .filter(|d| needs_progress(&s(&d.id), &d.data) && !has_progress(&d.data))
        .map(|d| d.id.clone())
        .collect();
    out.sort();
    out
}

/// The document an `update` would leave: the stored one under `data`.
fn as_written(ctx: &Ctx, op: &str, coll: &str, id: &PyValue, data: &PyValue) -> R<PyValue> {
    if op == "update"
        && let Some(m) = ctx.merged_over(coll, id, data)?
    {
        return Ok(m);
    }
    Ok(data.clone())
}

/// `check_task_progress(store, op, doc_id, data)`.
pub(crate) fn check_task_progress(ctx: &Ctx, op: &str, id: &PyValue, data: &PyValue) -> R<()> {
    if !matches!(data, PyValue::Dict(_)) {
        return Ok(());
    }
    let d = as_written(ctx, op, "items", id, data)?;
    if needs_progress(id, &d) && !has_progress(&d) {
        return refuse(format!(
            "item {}: every open task needs progress {{pct: 0-100}} (an estimate; steps alone do not count)",
            id_text(id)
        ));
    }
    Ok(())
}

/// `has_goal_projects(data)`: a non-blank `goal` and a non-empty list of non-blank `projects`.
pub(crate) fn has_goal_projects(d: &PyValue) -> bool {
    let goal = matches!(get(d, "goal"), Some(PyValue::Str(g)) if !strip(g).is_empty());
    let projects = matches!(get(d, "projects"), Some(PyValue::List(p)) if !p.is_empty()
        && p.iter().all(|x| matches!(x, PyValue::Str(t) if !strip(t).is_empty())));
    goal && projects
}

/// An open or running task.
pub(crate) fn open_running_task(d: &PyValue) -> bool {
    is_str(get(d, "kind"), "task")
        && ["open", "running"]
            .iter()
            .any(|st| is_str(get(d, "status"), st))
}

/// `check_task_goal_projects(store, op, doc_id, data)`.
pub(crate) fn check_task_goal_projects(ctx: &Ctx, op: &str, id: &PyValue, data: &PyValue) -> R<()> {
    if !matches!(data, PyValue::Dict(_)) {
        return Ok(());
    }
    let d = as_written(ctx, op, "items", id, data)?;
    if open_running_task(&d) && !has_goal_projects(&d) {
        return refuse(format!(
            "item {}: invalid_argument: every open task needs a non-empty `goal` and a non-empty `projects` list of \"owner/repo\" strings (\"architect\" for Architect-owned or cross-project work)",
            id_text(id)
        ));
    }
    Ok(())
}

/// `dsch.validate(coll, doc_id, data)`; a schema that cannot load is the script's traceback.
pub(crate) fn validate(ctx: &Ctx, coll: &str, id: &str, data: &PyValue) -> R<Vec<String>> {
    crate::dashboard::schema_check::script::validate(&ctx.schema_dir(), coll, id, &to_json(data))
        .map_err(|e| Fail::Raise(format!("dashboard_schema.SchemaError: {e}")))
}

/// `check_question(store, op, doc_id, data)`: the (merged) doc against `questions.schema.json`.
pub(crate) fn check_question(ctx: &Ctx, op: &str, id: &PyValue, data: &PyValue) -> R<()> {
    let mut d = data.clone();
    if op == "update" {
        let body = if matches!(data, PyValue::Dict(_)) {
            data.clone()
        } else {
            dict(vec![])
        };
        if let Some(m) = ctx.merged_over("questions", id, &body)? {
            d = m;
        }
    }
    if let PyValue::Dict(p) = &d {
        d = PyValue::Dict(p.iter().filter(|(k, _)| k != "version").cloned().collect());
    }
    let errs = validate(ctx, "questions", &id_text(id), &d)?;
    if errs.is_empty() {
        return Ok(());
    }
    refuse(format!(
        "questions write refused by schema: {}",
        errs.join("; ")
    ))
}

/// `check_settings_level(doc_id, data)`: `meta/settings.technical_level` is one of the six levels.
pub(crate) fn check_settings_level(ctx: &Ctx, id: &PyValue, data: &PyValue) -> R<()> {
    let Some(level) = get(data, "technical_level").filter(|_| is_str(Some(id), "settings")) else {
        return Ok(());
    };
    let body = dict(vec![("technical_level", level.clone())]);
    let errs: Vec<String> = validate(ctx, "meta", "settings", &body)?
        .into_iter()
        .filter(|e| e.contains(": /technical_level"))
        .collect();
    if errs.is_empty() {
        return Ok(());
    }
    refuse(format!(
        "meta/settings write refused by schema: {} (expected one of: {})",
        errs.join("; "),
        crate::technical_level::LEVELS.join(", ")
    ))
}

/// `check_site(store, op, doc_id, data)`: a `sessions` write's `site` block against the schema.
pub(crate) fn check_site(ctx: &Ctx, id: &PyValue, data: &PyValue) -> R<()> {
    if !has_key(data, "site") {
        return Ok(());
    }
    let filled = merge(
        data,
        &dict(vec![
            ("name", s("x")),
            ("state", s("idle")),
            ("status", s("")),
            ("summary", s("")),
            ("updated", s("2000-01-01T00:00:00Z")),
        ]),
    );
    let errs: Vec<String> = validate(ctx, "sessions", &id_text(id), &filled)?
        .into_iter()
        .filter(|e| e.contains(": /site"))
        .collect();
    if errs.is_empty() {
        return Ok(());
    }
    refuse(format!(
        "sessions write refused by schema: {}",
        errs.join("; ")
    ))
}

/// `re.match(r"[DA][0-9a-km-np-z]{2}$", id)`.
fn decision_or_action_id(id: &str) -> bool {
    let b = id.strip_suffix('\n').unwrap_or(id);
    let c: Vec<char> = b.chars().collect();
    c.len() == 3 && matches!(c[0], 'D' | 'A') && id_digit(c[1]) && id_digit(c[2])
}

/// Why: the page reads `recommended: true`, not the label marker.
/// What: `check_recommended(store, op, doc_id, data)` on an open D/A item with
/// object options: several flags or none and no single "(Recommended)" label
/// is refused; one such label gets the flag (a stderr notice), written back
/// into `data` (the merged options on an `update`).
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`recommended`).
pub(crate) fn check_recommended(ctx: &Ctx, op: &str, id: &PyValue, data: &mut PyValue) -> R<()> {
    if !matches!(data, PyValue::Dict(_)) || !decision_or_action_id(&super::text_or(Some(id))) {
        return Ok(());
    }
    let mut merged = data.clone();
    if op == "update" {
        match ctx.merged_over("items", id, data)? {
            Some(m) => merged = m,
            None => return Ok(()),
        }
    }
    let Some(PyValue::List(opts)) = get(&merged, "options") else {
        return Ok(());
    };
    let status_open = get(&merged, "status").is_none_or(|st| is_str(Some(st), "open"));
    if opts.is_empty() || !status_open || !opts.iter().all(|o| matches!(o, PyValue::Dict(_))) {
        return Ok(());
    }
    let mut opts = opts.clone();
    let flagged = opts
        .iter()
        .filter(|o| get(o, "recommended") == Some(&PyValue::Bool(true)))
        .count();
    let n = id_text(id);
    if flagged > 1 {
        return refuse(format!(
            "item {n}: {flagged} options have recommended: true; exactly one is allowed"
        ));
    }
    if flagged == 1 {
        return Ok(());
    }
    let label = |o: &PyValue| get(o, "label").map_or(String::new(), py_str).to_lowercase();
    let marked: Vec<usize> = (0..opts.len())
        .filter(|&i| label(&opts[i]).contains("(recommended)"))
        .collect();
    if marked.len() != 1 {
        let why = if marked.is_empty() {
            "no option labelled \"(Recommended)\""
        } else {
            "several options labelled \"(Recommended)\""
        };
        return refuse(format!(
            "item {n}: open decision has no option with recommended: true and {why}"
        ));
    }
    let i = marked[0];
    opts[i] = set(&opts[i], "recommended", PyValue::Bool(true));
    let shown = get(&opts[i], "label").map_or("None".to_owned(), repr);
    ctx.warn(&format!(
        "notice: item {n}: set recommended: true on option {shown} (labelled \"(Recommended)\")"
    ));
    // The flag lands in `data`'s own list, or (an update without options) in a copy of the stored one.
    *data = set(data, "options", PyValue::List(opts));
    Ok(())
}

/// `check_prefix_open(prefix)`: the retired Blocker prefix `A` is never minted.
pub(crate) fn check_prefix_open(prefix: &str) -> R<()> {
    if prefix == "A" {
        return refuse("prefix A is retired; use B (blockers)");
    }
    Ok(())
}

/// A batch op's `doc_id` as `dq.split` takes it: a non-string raises.
fn id_str(id: &PyValue) -> R<String> {
    match id {
        PyValue::Str(t) => Ok(t.clone()),
        v => Err(Fail::Raise(format!(
            "TypeError: expected string or bytes-like object, got {}",
            py_repr(crate::pyval::type_name(v))
        ))),
    }
}

/// Why: a hand-typed id landed on a live entry once (2026-10-20 Dd3).
/// What: `check_set_new(store, coll, doc_id, if_version, allow_id)`: a retired
/// `A` id under alias rules and a qualified id are refused; with no
/// `if_version`, an existing doc is a conflict, and a missing id-shaped one
/// warns unless `allow_id`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`set-conflict`, `set-new-id`).
pub(crate) fn check_set_new(
    ctx: &Ctx,
    coll: &PyValue,
    id: &PyValue,
    if_version: Option<&PyValue>,
    allow_id: bool,
) -> R<()> {
    let text = id_str(id)?;
    let c: Vec<char> = text.chars().collect();
    let retired = c.len() == 3 && c[0] == 'A' && id_digit(c[1]) && id_digit(c[2]);
    let coll_s = py_str(coll);
    if is_str(Some(coll), "items") && retired && !pyqual::alias_rules(&ctx.store)?.is_empty() {
        return refuse(format!("set {coll_s}/{text}: prefix A is retired; use B"));
    }
    if pyqual::split(&text).0.is_some() {
        return refuse(format!(
            "set {coll_s}/{text}: a qualified id is a reference, not a new id; use `create` or a bare id"
        ));
    }
    if if_version.is_some_and(|v| !matches!(v, PyValue::Null)) {
        return Ok(());
    }
    match ctx.store.get(coll, id) {
        Err(PyStoreError::NotFound(_)) => {
            if !allow_id && crate::dashboard::pyderive::ids::id_number(&text).is_some() {
                let first: String = text.chars().take(1).collect();
                ctx.warn(&format!(
                    "dashboard-db: warning: set {coll_s}/{text}: {text} does not exist; new ids come from `create {coll_s} {first} --json X` (lowest free id after the last issued). Pass --allow-id when restoring a known id"
                ));
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
        Ok(_) => Err(Fail::Store(PyStoreError::Conflict(format!(
            "{coll_s}/{text} already exists; pass --if-version <v> to replace it, or `create` for a new id"
        )))),
    }
}

/// `check_ops(ops, store)`: each `set`/`update` op's collection checks, in order.
pub(crate) fn check_ops(ctx: &Ctx, ops: &mut PyValue) -> R<()> {
    let PyValue::List(list) = ops else {
        return Ok(());
    };
    for op in list.iter_mut() {
        let kind = match get(op, "op") {
            Some(PyValue::Str(k)) if k == "set" || k == "update" => k.clone(),
            _ => continue,
        };
        let null = PyValue::Null;
        let id = get(op, "id").unwrap_or(&null).clone();
        let mut data = get(op, "data").unwrap_or(&null).clone();
        let coll = get(op, "collection");
        if is_str(coll, "meta") {
            check_settings_level(ctx, &id, &data)?;
        }
        if is_str(coll, "items") {
            check_steps(&id, &data)?;
            check_task_progress(ctx, &kind, &id, &data)?;
            check_task_goal_projects(ctx, &kind, &id, &data)?;
            super::warn::warn_no_task(ctx, &kind, &id, &data)?;
            check_recommended(ctx, &kind, &id, &mut data)?;
            if has_key(op, "data") {
                *op = set(op, "data", data);
            }
        } else if is_str(coll, "questions") {
            check_question(ctx, &kind, &id, &data)?;
        } else if is_str(coll, "sessions") {
            check_site(ctx, &id, &data)?;
        }
    }
    Ok(())
}

/// Every url an item carries in `links` (with grouped `sources`) and `specs`.
fn link_urls(d: &PyValue) -> Vec<String> {
    let mut out: Vec<Option<&PyValue>> = Vec::new();
    for k in ["links", "specs"] {
        let Some(PyValue::List(l)) = get(d, k) else {
            continue;
        };
        for x in l.iter().filter(|x| matches!(x, PyValue::Dict(_))) {
            out.push(get(x, "url"));
            out.push(get(x, "href"));
            if let Some(PyValue::List(src)) = get(x, "sources") {
                out.extend(
                    src.iter()
                        .filter(|y| matches!(y, PyValue::Dict(_)))
                        .map(|y| get(y, "url")),
                );
            }
        }
    }
    out.into_iter()
        .filter_map(|u| match u {
            Some(PyValue::Str(t)) if !strip(t).is_empty() => Some(strip(t).to_owned()),
            _ => None,
        })
        .collect()
}

/// `GH_TICKET.match(url)`: `https?://github.com/<owner>/<repo>/(issues|pull)/<digits>`.
fn gh_ticket(u: &str) -> bool {
    let Some(rest) = u
        .strip_prefix("https://github.com/")
        .or_else(|| u.strip_prefix("http://github.com/"))
    else {
        return false;
    };
    let seg = |t: &str| !t.is_empty() && !t.contains(crate::pystr::is_space);
    let mut parts = rest.splitn(4, '/');
    let (Some(owner), Some(repo), Some(kind), Some(tail)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    seg(owner)
        && seg(repo)
        && (kind == "issues" || kind == "pull")
        && tail.chars().next().is_some_and(crate::pystr::is_digit)
}

/// `task_link_warnings(docs)`: open tasks with no link at all, and delegated
/// tasks with no GitHub issue or PR link; both sorted.
pub(crate) fn task_link_warnings(docs: &[PyDoc]) -> (Vec<String>, Vec<String>) {
    let (mut none, mut noticket) = (Vec::new(), Vec::new());
    for d in docs.iter().filter(|d| open_running_task(&d.data)) {
        let urls = link_urls(&d.data);
        let brief = matches!(get(&d.data, "brief"), Some(PyValue::Str(b)) if !strip(b).is_empty());
        if urls.is_empty() && !brief {
            none.push(d.id.clone());
        }
        let own = matches!(get(&d.data, "projects"), Some(PyValue::List(p)) if !p.is_empty()
            && p.iter().all(|x| matches!(x, PyValue::Str(t) if OWN_PROJECTS.contains(&strip(t).to_lowercase().as_str()))));
        if !own && !urls.iter().any(|u| gh_ticket(u)) {
            noticket.push(d.id.clone());
        }
    }
    none.sort();
    noticket.sort();
    (none, noticket)
}
