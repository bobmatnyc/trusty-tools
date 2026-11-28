//! `dashboard-db.py`'s writes: set, update, create, delete, log, status-brief, batch (#12029 P2g).
//!
//! Why: every write goes through the store's one-op batch (one `flock`, the
//! version check, atomic replace), after the script's checks and
//! normalisation in the script's order, so a refused write changes nothing
//! and an accepted one is the frozen script's bytes.
//! What: [`load_json`], [`stamp_task`] (`progress_at` on a task write that
//! moves progress), [`set_or_update`], [`create`], [`delete`], [`append_log`]
//! and [`append_status_brief`] (both pinned to the version read, one retry
//! on a conflict) and [`batch`]. An `items` update that moves a document out
//! of `interactive` runs the questions kick after the store returns, so the
//! lock is released first; a kick error is a stderr line and never changes
//! the exit code (intended divergence: the script spawns `questions_rc.py`
//! detached).
//! Test: `dashboard_db_recordings_replay_clean_against_the_port`,
//! `kick_runs_after_the_write_and_never_changes_the_exit_code`,
//! `every_write_refusal_leaves_the_store_unchanged`.

use std::io::Read;

use super::checks::{
    check_prefix_open, check_question, check_recommended, check_set_new, check_settings_level,
    check_site, check_steps, check_task_goal_projects, check_task_progress, validate,
};
use super::normalize::{
    default_item_fields, has_specs, normalize_options, normalize_specs, normalize_subs,
    with_question_title,
};
use super::warn::{warn_level, warn_no_task};
use super::{Ctx, Fail, R, dict, merge, py_list, refuse, s, set};
use crate::dashboard::pyderive::{ids as rules, stale};
use crate::dashboard::pyids::{self, Ctx as IdsCtx, IdsError};
use crate::dashboard::pyqual;
use crate::dashboard::store::py::{OpOut, PyStoreError};
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::strip;
use crate::pyval::{get, is_str, truthy};

/// `load_json(arg)`: `-` reads stdin, text starting `{` or `[` is the JSON,
/// anything else is a file path.
pub(crate) fn load_json(arg: &str, stdin: &mut dyn Read) -> R<PyValue> {
    let text = if arg == "-" {
        let mut buf = Vec::new();
        stdin
            .read_to_end(&mut buf)
            .map_err(|e| Fail::Raise(format!("OSError: {e}")))?;
        decode(&buf, "<stdin>")?
    } else if strip(arg).starts_with(['{', '[']) {
        strip(arg).to_owned()
    } else {
        let bytes = std::fs::read(arg).map_err(|e| {
            Fail::Store(PyStoreError::Store(format!(
                "cannot read {arg}: {}",
                crate::writers::errno_text(&e, arg)
            )))
        })?;
        decode(&bytes, arg)?
    };
    loads(&text)
        .map_err(|e| Fail::Store(PyStoreError::Store(format!("invalid JSON: {}", e.py_str()))))
}

/// A text-mode read: UTF-8 and universal newlines; a bad byte is the script's traceback.
fn decode(bytes: &[u8], _what: &str) -> R<String> {
    let text = std::str::from_utf8(bytes).map_err(|e| {
        Fail::Raise(format!(
            "UnicodeDecodeError: 'utf-8' codec can't decode byte at position {}",
            e.valid_up_to()
        ))
    })?;
    Ok(crate::writers::universal_newlines(text.to_owned()))
}

/// The `pyids` context over this run's store root.
fn ids_ctx<'c>(ctx: &'c Ctx) -> IdsCtx<'c> {
    IdsCtx {
        cwd: ctx.o.cwd.clone(),
        default_root: ctx.default_root().to_string_lossy().into_owned(),
        env: ctx.o.env,
        schema_dir: ctx.schema_dir(),
        briefs: crate::writers::brief_ids::Paths {
            repo: ctx.o.checkout.clone(),
        },
        clock: ctx.o.clock.clone(),
    }
}

/// Why: `stale-tasks.py` reads `progress_at` as the time a task last moved.
/// What: `stamp_task(store, op, doc_id, data)`: `data` with `progress_at` now
/// when the write creates a task or moves its progress, status, steps or
/// outcome; a `set` that moves nothing keeps the stored `progress_at`; an
/// explicit `progress_at` wins.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`set-task`).
pub(crate) fn stamp_task(ctx: &Ctx, op: &str, id: &PyValue, data: PyValue) -> R<PyValue> {
    if !matches!(data, PyValue::Dict(_)) || super::has_key(&data, "progress_at") {
        return Ok(data);
    }
    let mut old = None;
    if op == "update" || op == "set" {
        match ctx.get("items", id) {
            Ok(doc) => old = Some(doc.data),
            Err(PyStoreError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let old = old.filter(|o| matches!(o, PyValue::Dict(_)));
    let merged = match &old {
        Some(o) if op == "update" && truthy(o) => merge(o, &data),
        _ => data.clone(),
    };
    // `stamped is not merged`: stamp_progress builds a new dict only for a task it stamps.
    let stamps = is_str(get(&merged, "kind"), "task")
        && old.as_ref().is_none_or(|o| stale::moved(o, &merged));
    if stamps {
        let stamped = stale::stamp_progress(old.as_ref(), &merged, ctx.now());
        if let Some(at) = get(&stamped, "progress_at") {
            return Ok(set(&data, "progress_at", at.clone()));
        }
    } else if get(&merged, "progress_at").is_some_and(truthy) {
        return Ok(data);
    }
    if op == "set"
        && let Some(at) = old
            .as_ref()
            .and_then(|o| get(o, "progress_at"))
            .filter(|v| truthy(v))
    {
        return Ok(set(&data, "progress_at", at.clone()));
    }
    Ok(data)
}

/// A write result as the store returns it: `{collection, id, version}` or `{..., deleted: true}`.
pub(crate) fn op_value(r: &OpOut) -> PyValue {
    let tail = match &r.version {
        Some(v) => ("version", PyValue::Int(v.clone())),
        None => ("deleted", PyValue::Bool(true)),
    };
    dict(vec![
        ("collection", s(&r.collection)),
        ("id", s(&r.id)),
        tail,
    ])
}

/// Why: `set` replaces a document and `update` merges into one, after the
/// collection's checks.
/// What: the script's `set`/`update` branch: resolve (update) or
/// [`check_set_new`] (set), load, check and normalize by collection, write,
/// then the questions kick when an `items` update leaves `interactive`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`set-*`, `update-*`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_or_update(
    ctx: &Ctx,
    cmd: &str,
    coll: &str,
    id: &str,
    json: &str,
    if_version: Option<&PyValue>,
    allow_id: bool,
    stdin: &mut dyn Read,
) -> R<PyValue> {
    let id = if cmd == "update" {
        pyqual::resolve(&ctx.store, coll, id, &ctx.default_root())?
    } else {
        check_set_new(ctx, &s(coll), &s(id), if_version, allow_id)?;
        id.to_owned()
    };
    let idv = s(&id);
    let mut data = load_json(json, stdin)?;
    match coll {
        "items" => {
            check_steps(&idv, &data)?;
            check_task_progress(ctx, cmd, &idv, &data)?;
            check_task_goal_projects(ctx, cmd, &idv, &data)?;
            warn_no_task(ctx, cmd, &idv, &data)?;
            check_recommended(ctx, cmd, &idv, &mut data)?;
            let title = get(&data, "title").map_or(String::new(), crate::pyval::py_str);
            if cmd == "update"
                && matches!(data, PyValue::Dict(_))
                && !has_specs(&data)
                && title.contains("http")
            {
                let stored = ctx.get("items", &idv)?.data;
                let keep = get(&stored, "specs")
                    .filter(|v| truthy(v))
                    .map_or_else(Vec::new, py_list);
                data = set(&data, "specs", PyValue::List(keep));
            }
            data = normalize_specs(data, &ctx.tail_host);
            warn_level(ctx, &id, &data);
            data = stamp_task(ctx, cmd, &idv, data)?;
        }
        "questions" => {
            if cmd == "set" {
                data = with_question_title(data)?;
            }
            check_question(ctx, cmd, &idv, &data)?;
        }
        "sessions" => check_site(ctx, &idv, &data)?,
        "meta" => check_settings_level(ctx, &idv, &data)?,
        _ => {}
    }
    let mut was = None;
    if cmd == "update" && coll == "items" {
        match ctx.get("items", &idv) {
            Ok(doc) => was = get(&doc.data, "status").cloned(),
            Err(PyStoreError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let collv = s(coll);
    let res = if cmd == "set" {
        ctx.store.set(&collv, &idv, Some(&data), if_version, None)
    } else {
        ctx.store
            .update(&collv, &idv, Some(&data), if_version, None)
    }?;
    ctx.sync();
    if left_interactive(was.as_ref(), &data) {
        kick(ctx);
    }
    Ok(op_value(&res))
}

/// `left_interactive(old, data)`: the update moves the doc out of `interactive`.
fn left_interactive(old: Option<&PyValue>, data: &PyValue) -> bool {
    is_str(old, "interactive")
        && get(data, "status").is_some_and(|st| !is_str(Some(st), "interactive"))
}

/// Why: Remote Control on the questions session goes off once the last
/// interactive question is answered (Bob 2026-11-17).
/// What: `kick_questions_rc()`: nothing under `QUESTIONS_RC_KICK=off`; else the
/// caller's in-process `questions::rc` pass, after the write's lock is gone.
/// A failure is one `questions_rc kick failed: <e>` stderr line.
/// Test: `kick_runs_after_the_write_and_never_changes_the_exit_code`.
fn kick(ctx: &Ctx) {
    // #12029 P2g: in process, never a detached python spawn (Architect ruling 17:10Z).
    if (ctx.o.env)("QUESTIONS_RC_KICK").as_deref() == Some("off") {
        return;
    }
    if let Err(e) = (ctx.o.kick)() {
        ctx.warn(&format!("questions_rc kick failed: {e}"));
    }
}

/// Why: a new item takes the lowest free id after the last issued, in one command.
/// What: the script's `create`: the next id, the `items` defaults, checks and
/// normalisation (or the `questions` title and schema check), a write with
/// `if_version` 0, then the counter file.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`create-*`).
pub(crate) fn create(
    ctx: &Ctx,
    coll: &str,
    prefix: &str,
    json: &str,
    stdin: &mut dyn Read,
) -> R<PyValue> {
    let n = next_id(ctx, prefix)?;
    let nv = s(&n);
    let mut data = load_json(json, stdin)?;
    if coll == "items" {
        data = default_item_fields(ctx, prefix, data)?;
        check_steps(&nv, &data)?;
        check_task_progress(ctx, "create", &nv, &data)?;
        check_task_goal_projects(ctx, "create", &nv, &data)?;
        warn_no_task(ctx, "create", &nv, &data)?;
        data = normalize_specs(normalize_subs(normalize_options(data)?)?, &ctx.tail_host);
        warn_level(ctx, &n, &data);
        check_recommended(ctx, "create", &nv, &mut data)?;
        data = stamp_task(ctx, "create", &nv, data)?;
        if matches!(data, PyValue::Dict(_)) && !get(&data, "updated").is_some_and(truthy) {
            let created = get(&data, "created").filter(|v| truthy(v)).cloned();
            data = set(
                &data,
                "updated",
                created.unwrap_or_else(|| s(&ctx.now_iso())),
            );
        }
    }
    if coll == "questions" {
        data = with_question_title(data)?;
        check_question(ctx, "create", &nv, &data)?;
    }
    let zero = PyValue::Int(crate::pyjson_load::PyInt::Small(0));
    let res = ctx
        .store
        .set(&s(coll), &nv, Some(&data), Some(&zero), None)?;
    ctx.sync();
    pyids::record_issued(&ids_ctx(ctx), prefix, &n, Some(&ctx.store.root)).map_err(
        |e| match e {
            IdsError::Value(m) => Fail::Raise(format!("ValueError: {m}")),
            IdsError::Store(e) => Fail::Store(e),
        },
    )?;
    Ok(op_value(&res))
}

/// `next_id(snapshot(store, ID_COLLECTIONS), prefix, counters.get(prefix))`, or "no free ids".
pub(crate) fn next_id(ctx: &Ctx, prefix: &str) -> R<String> {
    check_prefix_open(prefix)?;
    let snap = ctx.snapshot(&rules::ID_COLLECTIONS)?;
    let last = pyids::read_counters(&ids_ctx(ctx), Some(&ctx.store.root))
        .into_iter()
        .find(|(k, _)| k == prefix)
        .map(|(_, v)| v);
    match rules::next_id(&snap, prefix, last) {
        Some(n) => Ok(n),
        None => refuse(format!("no free {prefix} ids")),
    }
}

/// `store.delete(coll, dq.resolve(store, coll, id), if_version)`.
pub(crate) fn delete(ctx: &Ctx, coll: &str, id: &str, if_version: Option<&PyValue>) -> R<PyValue> {
    let local = pyqual::resolve(&ctx.store, coll, id, &ctx.default_root())?;
    let res = ctx.store.delete(&s(coll), &s(&local), if_version, None)?;
    ctx.sync();
    Ok(op_value(&res))
}

/// One version-pinned `update` of `fields`, retried once on a conflict with a fresh read.
fn pinned_update(
    ctx: &Ctx,
    coll: &str,
    id: &str,
    fields: &dyn Fn(&PyValue) -> R<PyValue>,
) -> R<PyValue> {
    let (collv, idv) = (s(coll), s(id));
    for attempt in 0..2 {
        let doc = ctx.store.get(&collv, &idv)?;
        let data = if matches!(doc.data, PyValue::Dict(_)) {
            doc.data.clone()
        } else {
            PyValue::Null
        };
        let body = fields(&data)?;
        let version = PyValue::Int(doc.version.clone());
        match ctx
            .store
            .update(&collv, &idv, Some(&body), Some(&version), None)
        {
            Ok(r) => {
                ctx.sync();
                return Ok(op_value(&r));
            }
            Err(PyStoreError::Conflict(_)) if attempt == 0 => ctx.sync(),
            Err(e) => return Err(e.into()),
        }
    }
    refuse("update retried without a result")
}

/// Why: short accomplishment lines on a task (Bob 2026-10-28).
/// What: `append_log(store, collection, id, text)`: `{at, text}` appended to
/// `log`, `progress_at` set to the same instant, version-pinned.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`log`).
pub(crate) fn append_log(ctx: &Ctx, coll: &str, id: &str, text: &str) -> R<PyValue> {
    if coll != "items" {
        return refuse("log applies to the items collection");
    }
    let text = strip(text).to_owned();
    if text.is_empty() {
        return refuse("log text is empty");
    }
    let at = ctx.now_iso();
    let entry = dict(vec![("at", s(&at)), ("text", s(&text))]);
    let local = pyqual::resolve(&ctx.store, coll, id, &ctx.default_root())?;
    pinned_update(ctx, coll, &local, &|data| {
        let mut cur = match get(data, "log") {
            Some(PyValue::List(l)) => l.clone(),
            _ => Vec::new(),
        };
        cur.push(entry.clone());
        Ok(dict(vec![
            ("log", PyValue::List(cur)),
            ("progress_at", s(&at)),
        ]))
    })
}

/// Why: a task's status briefs are listed newest first.
/// What: `append_status_brief(store, id, brief_id, brief)`: `{id, at, brief}`
/// at the front of `status_briefs` (a repeated id moves), the new list
/// checked against the items schema, version-pinned.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`status-brief`).
pub(crate) fn append_status_brief(ctx: &Ctx, id: &str, brief_id: &str, brief: &str) -> R<PyValue> {
    let entry = dict(vec![
        ("id", s(strip(brief_id))),
        ("at", s(&ctx.now_iso())),
        ("brief", s(strip(brief))),
    ]);
    let local = pyqual::resolve(&ctx.store, "items", id, &ctx.default_root())?;
    pinned_update(ctx, "items", &local, &|data| {
        let cur = match get(data, "status_briefs") {
            Some(PyValue::List(l)) => l.clone(),
            _ => Vec::new(),
        };
        let same = |e: &PyValue| {
            matches!(e, PyValue::Dict(_))
                && crate::dashboard::pyderive::py_eq(get(e, "id"), get(&entry, "id"))
        };
        let mut list = vec![entry.clone()];
        list.extend(cur.into_iter().filter(|e| !same(e)));
        let merged = set(data, "status_briefs", PyValue::List(list.clone()));
        let errs: Vec<String> = validate(ctx, "items", &local, &merged)?
            .into_iter()
            .filter(|e| e.contains(": /status_briefs"))
            .collect();
        if !errs.is_empty() {
            return refuse(errs.join("; "));
        }
        Ok(dict(vec![("status_briefs", PyValue::List(list))]))
    })
}

/// Why: several writes in one all-or-nothing batch, with the same checks.
/// What: the script's `batch`: load (a list, or `{ops: [...]}`), [`check_ops`],
/// normalize and stamp each `items` set/update, [`check_set_new`] each `set`,
/// then `store.batch`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`batch-*`).
///
/// [`check_ops`]: super::checks::check_ops
pub(crate) fn batch(ctx: &Ctx, file: &str, stdin: &mut dyn Read) -> R<PyValue> {
    let loaded = load_json(file, stdin)?;
    let mut ops = match get(&loaded, "ops") {
        Some(o) if matches!(loaded, PyValue::Dict(_)) => o.clone(),
        _ => loaded,
    };
    super::checks::check_ops(ctx, &mut ops)?;
    if let PyValue::List(list) = &mut ops {
        for op in list.iter_mut() {
            let kind = match get(op, "op") {
                Some(PyValue::Str(k))
                    if (k == "set" || k == "update") && is_str(get(op, "collection"), "items") =>
                {
                    k.clone()
                }
                _ => continue,
            };
            let null = PyValue::Null;
            let id = get(op, "id").unwrap_or(&null).clone();
            let data = normalize_specs(get(op, "data").unwrap_or(&null).clone(), &ctx.tail_host);
            let data = stamp_task(ctx, &kind, &id, data)?;
            *op = set(op, "data", data);
        }
        for op in list.iter() {
            if is_str(get(op, "op"), "set")
                && let (Some(coll), Some(id)) = (get(op, "collection"), get(op, "id"))
            {
                check_set_new(ctx, coll, id, get(op, "if_version"), true)?;
            }
        }
    }
    let res = ctx.store.batch(&ops, None)?;
    ctx.sync();
    Ok(PyValue::List(res.iter().map(op_value).collect()))
}
