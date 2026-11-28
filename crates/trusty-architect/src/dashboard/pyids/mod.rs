//! The port of `dashboard_ids.py`: the root-based id wrappers, the issue counter and retirement (#12029 P2d).
//!
//! Why: `dashboard-db.py next` / `create` and the Architect's retire pass
//! read the id space from a store root, remember the last id `create`
//! issued (so retiring the top id cannot hand it out again at once), and
//! retire finished items by one fixed rule. Later slices port those callers
//! onto this module; the id rules themselves are [`super::pyderive::ids`].
//! What: over a [`PyStore`] root (the default root when none is given):
//! [`used`], [`free_count`], [`next_id`] (with the `.id-counters.json`
//! issue point), [`read_counters`] / [`record_issued`] (the counter file:
//! two base-34 digits per prefix; a bare `int` below 256 is a hex-era
//! counter read as its hex digits), [`brief_next`] / [`brief_used`] (the
//! `B` space of [`crate::writers::brief_ids`]), [`retire_candidates`],
//! [`retire_plan`] (every finished item over seven days, then the 289-free
//! floor, then the usage pass) and [`retire_apply`] (archive line, fsync,
//! then a delete with `if_version`; a failure skips that item). The command
//! line is in `cli`. Live writes: the counter file and the retire deletes.
//! Known gaps: a version past `u64` reads as 1 (as [`PyDoc`] reads it); a
//! `now` with a non-UTC offset stamps its UTC time.
//! Test: `dashboard_ids_recordings_replay_clean_against_the_port`,
//! `a_failed_archive_skips_the_item_and_keeps_it`.

mod cases;
pub(crate) mod cli;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use cases::replay_cases;

use super::pyderive::ids::{self as rules, ID_COLLECTIONS, KEEP_FREE, Retire, id_number};
use super::pyderive::{PySnap, coll, text_or, ts};
use super::store::py::{
    Options, PyStore, PyStoreError, abspath, dirname, makedirs, sorted, with_key,
};
use crate::FakeClock;
use crate::clock::Clock;
use crate::dashboard::pyjsonl::join;
use crate::ids36;
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::strip;
use crate::pyval::{dumps, get, is_str, py_str};
use crate::tasks::store_read::PyDoc;
use crate::writers::brief_ids;

/// The prefixes the retire pass walks, in order (`PREFIXES`).
pub(crate) const PREFIXES: &str = "GPADTSQ";
/// Finished items older than this many days always retire (`AGE_DAYS`).
pub(crate) const AGE_DAYS: f64 = 7.0; // `str()` is `7`: see `Plan::age_text`
/// The archive file under `<root>/../owner-queue-archive` (`RETIRED_LOG`).
const RETIRED_LOG: &str = "retired-ids.jsonl";
/// An answer this old counts as relayed for the usage pass (`RELAY_ASSUMED_HOURS`).
const RELAY_ASSUMED_HOURS: i64 = 48;
/// The usage pass's `reason` (`USAGE_REASON`).
pub(crate) const USAGE_REASON: &str = "usage pass";

/// A `ValueError` the module raises, or a store error it lets through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum IdsError {
    /// `ValueError` (a bad counter digit).
    #[error("{0}")]
    Value(String),
    /// A `StoreError` or `OSError`.
    #[error(transparent)]
    Store(#[from] PyStoreError),
}

/// An aware instant, as Unix seconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct At {
    pub(crate) unix: f64,
}

impl At {
    /// `now.strftime("%Y-%m-%dT%H:%M:%SZ")`.
    fn stamp(self) -> String {
        crate::classify::alerts::iso(self.unix.floor())
    }
}

/// What the module reads besides its arguments: `ds.DEFAULT_ROOT`, the
/// working directory (`abspath`), the environment, the schema directory,
/// the brief registry paths and the clock.
pub(crate) struct Ctx<'a> {
    pub(crate) cwd: PathBuf,
    pub(crate) default_root: String,
    pub(crate) env: &'a dyn Fn(&str) -> Option<String>,
    pub(crate) schema_dir: PathBuf,
    pub(crate) briefs: brief_ids::Paths,
    pub(crate) clock: Arc<dyn Clock + Send + Sync>,
}

impl Ctx<'_> {
    /// `root or ds.DEFAULT_ROOT`.
    fn root<'r>(&'r self, root: Option<&'r str>) -> &'r str {
        root.filter(|r| !r.is_empty()).unwrap_or(&self.default_root)
    }

    /// `ds.Store(root or ds.DEFAULT_ROOT, actor=actor)` on `clock`.
    pub(crate) fn store(
        &self,
        root: Option<&str>,
        actor: Option<&str>,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<PyStore, PyStoreError> {
        let opts = Options {
            actor: actor.map(str::to_owned),
            ..Options::default()
        };
        PyStore::new(
            self.root(root),
            opts,
            self.env,
            &self.cwd,
            self.schema_dir.clone(),
            clock,
        )
    }

    /// `_snap(root)`.
    pub(crate) fn snap(&self, root: Option<&str>) -> Result<PySnap, PyStoreError> {
        snapshot(&self.store(root, None, self.clock.clone())?)
    }
}

/// `derive.snapshot(store, COLLECTIONS)`.
pub(crate) fn snapshot(store: &PyStore) -> Result<PySnap, PyStoreError> {
    let mut snap = PySnap::new();
    for c in ID_COLLECTIONS {
        let docs = store.list_docs(&PyValue::Str(c.into()), None, None, false, None)?;
        let docs = docs.into_iter().map(|d| PyDoc {
            id: d.id,
            data: d.data,
            version: match d.version {
                PyInt::Small(v) => u64::try_from(v).unwrap_or(1),
                PyInt::Big(_) => 1,
            },
        });
        snap.insert(c.to_owned(), docs.collect());
    }
    Ok(snap)
}

/// `used(prefix, root)`.
pub(crate) fn used(ctx: &Ctx, prefix: &str, root: Option<&str>) -> Result<Vec<i64>, PyStoreError> {
    Ok(rules::used_ids(&ctx.snap(root)?, prefix)
        .into_iter()
        .collect())
}

/// `free_count(prefix, root)`.
pub(crate) fn free_count(ctx: &Ctx, prefix: &str, root: Option<&str>) -> Result<i64, PyStoreError> {
    Ok(rules::free_count(&ctx.snap(root)?, prefix))
}

/// `counters_path(root)`: `<abspath(root)>/.id-counters.json`.
pub(crate) fn counters_path(ctx: &Ctx, root: Option<&str>) -> String {
    join(&abspath(ctx.root(root), &ctx.cwd), ".id-counters.json")
}

/// Why: the counter file keeps `create`'s last id per prefix across retirements.
/// What: `read_counters(root)`: `{prefix: number}` in file order; a string
/// is two base-34 digits, an `int` in `0..256` is a hex-era counter read
/// as its two hex digits (`255` is `ff`, 525); anything else is skipped.
/// An absent, unreadable or non-object file is empty.
/// Test: `dashboard_ids_recordings_replay_clean_against_the_port` (`functions`).
pub(crate) fn read_counters(ctx: &Ctx, root: Option<&str>) -> Vec<(String, i64)> {
    // #12029: every failure is `{}`, as the prototype's except clause.
    let Ok(text) = read_text(Path::new(&counters_path(ctx, root))) else {
        return Vec::new();
    };
    let Ok(PyValue::Dict(raw)) = loads(&text) else {
        return Vec::new();
    };
    let mut out: Vec<(String, i64)> = Vec::new();
    for (k, v) in raw {
        let n = match v {
            PyValue::Int(PyInt::Small(i)) if (0..256).contains(&i) => {
                ids36::dec(&format!("{i:02x}")).ok()
            }
            PyValue::Str(s) => ids36::dec(&s).ok(),
            _ => None,
        };
        if let Some(n) = n.and_then(|n| i64::try_from(n).ok()) {
            match out.iter_mut().find(|(key, _)| *key == k) {
                Some(slot) => slot.1 = n,
                None => out.push((k, n)),
            }
        }
    }
    out
}

/// Why: retiring the top id must not make it the next id at once.
/// What: `record_issued(prefix, doc_id, root)`: the counters re-encoded,
/// `prefix` set to `doc_id`'s digits (checked first), written to a `.tmp`
/// beside the file, fsynced and renamed (`json.dump(sort_keys=True)`).
/// Test: `dashboard_ids_recordings_replay_clean_against_the_port` (`functions`).
///
/// # Errors
/// [`IdsError::Value`] for digits that are not two base-34 digits (nothing
/// is written); the `OSError` of the directory or the write.
pub(crate) fn record_issued(
    ctx: &Ctx,
    prefix: &str,
    doc_id: &str,
    root: Option<&str>,
) -> Result<(), IdsError> {
    let mut c: Vec<(String, PyValue)> = read_counters(ctx, root)
        .into_iter()
        .map(|(k, v)| (k, PyValue::Str(ids36::enc(v).unwrap_or_default())))
        .collect();
    let digits: String = doc_id.chars().skip(1).collect();
    ids36::dec(&digits).map_err(|e| IdsError::Value(e.to_string()))?;
    match c.iter_mut().find(|(k, _)| k == prefix) {
        Some(slot) => slot.1 = PyValue::Str(digits),
        None => c.push((prefix.to_owned(), PyValue::Str(digits))),
    }
    c.sort_by(|a, b| a.0.cmp(&b.0));
    let path = counters_path(ctx, root);
    makedirs(dirname(&path))?;
    let tmp = format!("{path}.tmp");
    write_synced(&tmp, dumps(&PyValue::Dict(c), None, true).as_bytes(), false)?;
    std::fs::rename(&tmp, &path).map_err(|e| PyStoreError::os(&tmp, &e))?;
    Ok(())
}

/// Write (or append) `payload` to `path` and fsync it.
fn write_synced(path: &str, payload: &[u8], append: bool) -> Result<(), PyStoreError> {
    use std::io::Write;
    let os = |e: std::io::Error| PyStoreError::os(path, &e);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .map_err(os)?;
    f.write_all(payload).map_err(os)?;
    f.sync_all().map_err(os)
}

/// `next_id(prefix, root)`: [`rules::next_id`] after the counter's issue point.
pub(crate) fn next_id(
    ctx: &Ctx,
    prefix: &str,
    root: Option<&str>,
) -> Result<Option<String>, PyStoreError> {
    let snap = ctx.snap(root)?;
    let last = read_counters(ctx, root)
        .into_iter()
        .find(|(k, _)| k == prefix)
        .map(|(_, v)| v);
    Ok(rules::next_id(&snap, prefix, last))
}

/// `brief_used()`: how many brief ids are taken.
pub(crate) fn brief_used(ctx: &Ctx) -> Result<usize, String> {
    brief_ids::used(&ctx.briefs).map(|u| u.len())
}

/// `brief_next()`: the next brief id; brief ids never wrap.
pub(crate) fn brief_next(ctx: &Ctx) -> Result<String, String> {
    brief_ids::allocate(&ctx.briefs)
}

/// `retire_candidates(prefix, root, keep_free, age_days, now)`.
pub(crate) fn retire_candidates(
    ctx: &Ctx,
    prefix: &str,
    root: Option<&str>,
    r: Retire,
) -> Result<Vec<String>, PyStoreError> {
    Ok(rules::retire_candidates(
        &ctx.snap(root)?,
        prefix,
        r,
        ctx.clock.as_ref(),
    ))
}

/// `_usage_candidates(snap, prefix, taken, need, now)`: answered or relayed
/// items that count as relayed, oldest answer first, at most `need`.
fn usage_candidates(
    snap: &PySnap,
    prefix: &str,
    taken: &[String],
    need: i64,
    now: At,
) -> Vec<String> {
    let cut = (now.unix * 1e6).round() as i64 - RELAY_ASSUMED_HOURS * 3_600_000_000;
    let floor = crate::classify::alerts::days_from_civil(1, 1, 1) * 86_400_000_000;
    let mut rows: Vec<(i64, i64)> = Vec::new();
    for doc in coll(snap, "items") {
        let Some((p, n)) = id_number(&doc.id) else {
            continue;
        };
        if p.to_string() != prefix || taken.contains(&doc.id) {
            continue;
        }
        let d = &doc.data;
        let st = get(d, "status");
        if !(is_str(st, "answered") || is_str(st, "relayed"))
            || get(d, "keep") == Some(&PyValue::Bool(true))
        {
            continue;
        }
        let ans = ts(get(d, "answered_at"));
        let relayed = !strip(&text_or(d, "relayed_at")).is_empty() || is_str(st, "relayed");
        if !(relayed || ans.is_some_and(|a| a.utc < cut)) {
            continue;
        }
        let at = ans
            .or_else(|| ts(get(d, "relayed_at")))
            .or_else(|| ts(get(d, "updated")))
            .map_or(floor, |t| t.utc);
        rows.push((at, n));
    }
    rows.sort();
    let need = usize::try_from(need.max(0)).unwrap_or(0);
    rows.into_iter()
        .take(need)
        .map(|(_, n)| format!("{prefix}{}", ids36::enc(n).unwrap_or_default()))
        .collect()
}

/// One retire row: `{collection, id, version, title, status, reason}`.
fn row(coll_name: &str, doc: &PyDoc, reason: String) -> PyValue {
    let d = &doc.data;
    let title = match get(d, "title").filter(|v| crate::pyval::truthy(v)) {
        Some(t) => py_str(t),
        None => text_or(d, "asked"),
    };
    PyValue::Dict(vec![
        ("collection".into(), PyValue::Str(coll_name.into())),
        ("id".into(), PyValue::Str(doc.id.clone())),
        (
            "version".into(),
            PyValue::Int(PyInt::Small(i64::try_from(doc.version).unwrap_or(1))),
        ),
        (
            "title".into(),
            PyValue::Str(title.chars().take(200).collect()),
        ),
        (
            "status".into(),
            get(d, "status").cloned().unwrap_or(PyValue::Null),
        ),
        ("reason".into(), PyValue::Str(reason)),
    ])
}

/// The retire rule's knobs (`retire_plan`'s keyword arguments).
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub(crate) prefixes: String,
    pub(crate) keep_free: i64,
    pub(crate) age_days: f64,
    /// `str(age_days)` for the `reason` (`7`, `0.5`).
    pub(crate) age_text: String,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            prefixes: PREFIXES.into(),
            keep_free: KEEP_FREE,
            age_days: AGE_DAYS,
            age_text: "7".into(),
        }
    }
}

/// Why: retirement is one fixed rule, a pure function of `(snapshot, now)`.
/// What: `retire_plan(snap, now, prefixes, keep_free, age_days)`: per
/// prefix, the finished items over `age_days` and the oldest finished ones
/// to the floor (one row per collection holding the id), then the usage
/// pass when the prefix is still short.
/// Test: `dashboard_ids_recordings_replay_clean_against_the_port` (`retire`, `floor`).
pub(crate) fn retire_plan(snap: &PySnap, now: At, plan: &Plan) -> Vec<(String, Vec<PyValue>)> {
    let fixed = FakeClock::new(now.unix);
    let rule = |keep_free| Retire {
        keep_free,
        age_days: Some(plan.age_days),
        now: Some(now.unix),
    };
    let mut out = Vec::new();
    for p in plan.prefixes.chars().map(String::from) {
        let old = rules::retire_candidates(snap, &p, rule(0), &fixed);
        let picked = rules::retire_candidates(snap, &p, rule(plan.keep_free), &fixed);
        let mut rows = Vec::new();
        for cid in &picked {
            let reason = if old.contains(cid) {
                format!("done over {} days", plan.age_text)
            } else {
                format!("{}-free floor", plan.keep_free)
            };
            for c in ID_COLLECTIONS {
                for doc in coll(snap, c).iter().filter(|d| d.id == *cid) {
                    rows.push(row(c, doc, reason.clone()));
                }
            }
        }
        let short = plan.keep_free - rules::free_count(snap, &p) - picked.len() as i64;
        if short > 0 {
            for cid in usage_candidates(snap, &p, &picked, short, now) {
                if let Some(doc) = coll(snap, "items").iter().find(|d| d.id == cid) {
                    rows.push(row("items", doc, USAGE_REASON.into()));
                }
            }
        }
        out.push((p, rows));
    }
    out
}

/// `retire_apply`'s answer for one prefix: `(prefix, planned, retired, skipped, free_after)`.
pub(crate) type Applied = (String, i64, i64, i64, i64);

/// Why: the plan must be run without ever losing an item it could not archive.
/// What: `retire_apply(root, apply, now, prefixes)`. A dry run writes
/// nothing. With `apply`, each row is appended (fsynced) to
/// `<archive>/retired-ids.jsonl` with `retired_at`, then deleted with
/// `if_version` under actor `dashboard_ids retire`; an archive or delete
/// failure skips that row with a stderr line and the run goes on.
/// Test: `dashboard_ids_recordings_replay_clean_against_the_port` (`retire`),
/// `a_failed_archive_skips_the_item_and_keeps_it`.
///
/// # Errors
/// The store's own errors (a bad root, an unreadable document).
pub(crate) fn retire_apply(
    ctx: &Ctx,
    root: Option<&str>,
    apply: bool,
    now: Option<At>,
    prefixes: &str,
    stderr: &mut String,
) -> Result<Vec<Applied>, PyStoreError> {
    let now = now.unwrap_or(At {
        unix: ctx.clock.now_unix(),
    });
    let stamp = now.stamp();
    let store = ctx.store(
        root,
        Some("dashboard_ids retire"),
        Arc::new(FakeClock::new(now.unix)),
    )?;
    let archive = join(
        dirname(&abspath(ctx.root(root), &ctx.cwd)),
        "owner-queue-archive",
    );
    let plan = Plan {
        prefixes: prefixes.into(),
        ..Plan::default()
    };
    let mut out = Vec::new();
    for (p, rows) in retire_plan(&snapshot(&store)?, now, &plan) {
        let (mut done, mut skipped) = (0, 0);
        for r in rows.iter().filter(|_| apply) {
            // #12029: a failed archive or delete skips the row, never deletes unarchived.
            match archive_then_delete(&store, &archive, r, &stamp) {
                Ok(()) => done += 1,
                Err(e) => {
                    skipped += 1;
                    let (c, id) = (text_or(r, "collection"), text_or(r, "id"));
                    stderr.push_str(&format!("dashboard_ids retire: {c}/{id} skipped: {e}\n"));
                }
            }
        }
        let free = rules::free_count(&snapshot(&store)?, &p);
        out.push((p, rows.len() as i64, done, skipped, free));
    }
    Ok(out)
}

/// The archive line (fsynced), then the versioned delete.
fn archive_then_delete(
    store: &PyStore,
    archive: &str,
    r: &PyValue,
    stamp: &str,
) -> Result<(), PyStoreError> {
    let path = join(archive, RETIRED_LOG);
    makedirs(dirname(&path))?;
    let pairs = match r {
        PyValue::Dict(p) => p.as_slice(),
        _ => &[],
    };
    let line = sorted(&PyValue::Dict(with_key(
        pairs,
        "retired_at",
        PyValue::Str(stamp.into()),
    )));
    write_synced(
        &path,
        format!("{}\n", dumps(&line, None, false)).as_bytes(),
        true,
    )?;
    let field = |k: &str| get(r, k).cloned().unwrap_or(PyValue::Null);
    store.delete(&field("collection"), &field("id"), get(r, "version"), None)?;
    Ok(())
}
