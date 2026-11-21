//! Pruning, archive reads and log rotation.
//!
//! Why: closed items leave `state.json` for `archive.jsonl` so the state stays
//! under its size bar; the log is rotated weekly into `log-<day>.jsonl`; an
//! archive line whose commit never landed is an orphan that the next lock drops.
//! What: [`prune_state`] and [`prune_reminders`] (pure, on the in-memory state),
//! [`reconcile_archive`] (drops `cv > v` lines), [`archive_lookup`] and
//! [`rotate_plan`] (which lines move). The file writes run under the lock in
//! `ledger.rs`.
//! Test: `prune_state_moves_done_and_old_answered`, `prune_reminders_keeps_the_top_number`,
//! `reconcile_drops_orphans_only`, `archive_lookup_skips_orphans`, `rotate_plan_splits_by_age`.

use super::error::{IlError, IlResult, invalid};
use super::json::Json;
use super::log::log_rec;
use super::store::{Paths, atomic_write, load_state};
use super::time::{Shape, strptime};
use crate::pystr::splitlines;
use std::collections::HashMap;
use std::fs;

/// Hours a finished reminder stays before it is archived.
pub const REMINDER_KEEP_HOURS: f64 = 48.0;

/// `(kind, object)` pairs leaving the state for the archive.
pub type Gone = Vec<(String, Json)>;

/// `prune_reminders`: finished reminders (done, `at` over 48 h past, not the highest number).
pub fn prune_reminders(st: &mut Json, now: f64) -> Gone {
    let mut out = Gone::new();
    for t in st.arr_or_insert("tasks").iter_mut() {
        let tid = t.s("id").to_owned();
        let rs: Vec<Json> = t.arr("reminders").to_vec();
        let top = rs.iter().filter_map(|r| r.i("n")).max().unwrap_or(0);
        let mut keep = Vec::new();
        for r in &rs {
            let age = strptime(r.s("at"), Shape::Minute).map_or(0.0, |a| now - a as f64);
            if r.s("state") == "done" && age > REMINDER_KEEP_HOURS * 3600.0 && r.i("n") != Some(top)
            {
                let mut item = r.clone();
                item.set("id", format!("{tid}#{}", r.i("n").unwrap_or(0)));
                item.set("task", tid.clone());
                out.push(("reminder".to_owned(), item));
            } else {
                keep.push(r.clone());
            }
        }
        if keep.len() != rs.len() {
            t.set("reminders", Json::Arr(keep));
        }
    }
    out
}

/// `prune_state`: removes done items and tasks, answered items older than `hours`, and finished reminders.
pub fn prune_state(
    st: &mut Json,
    times: &HashMap<String, i64>,
    hours: f64,
    require_outcome: bool,
    now: f64,
) -> Gone {
    let mut out = Gone::new();
    let mut keep = Vec::new();
    for o in std::mem::take(st.arr_or_insert("items")) {
        let old = times
            .get(o.s("id"))
            .is_none_or(|t| now - *t as f64 >= hours * 3600.0);
        let has_outcome = o.get("outcome").is_some_and(Json::truthy);
        if o.s("status") == "done"
            || (o.s("status") == "answered" && old && (has_outcome || !require_outcome))
        {
            out.push(("item".to_owned(), o));
        } else {
            keep.push(o);
        }
    }
    *st.arr_or_insert("items") = keep;
    let (done, rest): (Vec<Json>, Vec<Json>) = std::mem::take(st.arr_or_insert("tasks"))
        .into_iter()
        .partition(|o| o.s("status") == "done");
    *st.arr_or_insert("tasks") = rest;
    out.extend(done.into_iter().map(|o| ("task".to_owned(), o)));
    out.extend(prune_reminders(st, now));
    out
}

/// `cv` of one archive line: a quick read of the trailing `"cv":N}`, else a full parse.
fn line_cv(line: &str) -> Option<i64> {
    let body = line.trim_end();
    let Some(at) = body.rfind("\"cv\":") else {
        return if body.contains("\"cv\"") {
            Json::parse(body).ok().and_then(|j| j.i("cv"))
        } else {
            None
        };
    };
    let tail = &body[at + 5..];
    match tail
        .strip_suffix('}')
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        Some(n) => n.parse().ok(),
        None => Json::parse(body).ok().and_then(|j| j.i("cv")),
    }
}

/// `reconcile_archive`: drops archive lines whose commit never landed (`cv` above the state's `v`).
/// The caller holds the lock. An unreadable state or archive is left alone.
///
/// # Errors
/// `Io` when the rewrite fails.
pub fn reconcile_archive(paths: &Paths, fail_rename: bool) -> IlResult<()> {
    let v = fs::read_to_string(&paths.state)
        .ok()
        .and_then(|t| Json::parse(&t).ok())
        .and_then(|j| j.i("v"));
    let (Some(v), Ok(text)) = (v, fs::read_to_string(&paths.archive)) else {
        return Ok(());
    };
    let keep: Vec<&str> = text
        .split_inclusive('\n')
        .filter(|l| line_cv(l).is_none_or(|cv| cv <= v))
        .collect();
    if keep.len() != text.split_inclusive('\n').count() {
        atomic_write(&paths.archive, keep.concat().as_bytes(), fail_rename)?;
    }
    Ok(())
}

/// `archive_lookup`: the last valid archive line for `id`, printed as `json.dumps(hit)`.
///
/// # Errors
/// `Invalid` when the id is not archived.
pub fn archive_lookup(paths: &Paths, id: &str) -> IlResult<String> {
    let v = load_state(paths).ok().and_then(|s| s.i("v"));
    let text = fs::read_to_string(&paths.archive).unwrap_or_default();
    let mut hit: Option<Json> = None;
    for ln in text.lines().filter(|l| l.contains(id)) {
        let Ok(r) = Json::parse(ln) else { continue };
        let orphan = matches!((v, r.i("cv")), (Some(v), Some(cv)) if cv > v);
        if r.get("item").is_some_and(|i| i.s("id") == id) && !orphan {
            hit = Some(r);
        }
    }
    match hit {
        Some(h) => Ok(h.dump_spaced()),
        None => invalid(format!("{id} not in archive.jsonl")),
    }
}

/// `op_rotate` planning: the log's lines split into those older than the cutoff and the rest.
///
/// # Errors
/// `Corrupt` when the log is not UTF-8 (nothing is changed).
pub fn rotate_plan(
    log: &std::path::Path,
    cutoff: i64,
) -> IlResult<Option<(Vec<String>, Vec<String>)>> {
    let bytes = match fs::read(log) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let text = String::from_utf8(bytes).map_err(|e| {
        IlError::Corrupt(format!(
            "{} is not valid UTF-8 ({e}); nothing changed",
            log.display()
        ))
    })?;
    let (mut old, mut keep) = (Vec::new(), Vec::new());
    for ln in splitlines(&text) {
        match log_rec(ln).and_then(|r| r.t) {
            Some(t) if t < cutoff => old.push(ln.to_owned()),
            _ => keep.push(ln.to_owned()),
        }
    }
    Ok(Some((old, keep)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::json::obj;

    fn item(id: &str, status: &str) -> Json {
        obj([
            ("id", id.into()),
            ("status", status.into()),
            ("outcome", "".into()),
        ])
    }

    #[test]
    fn prune_state_moves_done_and_old_answered() {
        let mut st = obj([
            (
                "items",
                Json::Arr(vec![
                    item("a", "done"),
                    item("b", "answered"),
                    item("c", "answered"),
                    item("d", "open"),
                ]),
            ),
            (
                "tasks",
                Json::Arr(vec![item("t1", "done"), item("t2", "open")]),
            ),
        ]);
        let times = HashMap::from([("c".to_owned(), 1000)]);
        let gone = prune_state(&mut st, &times, 24.0, false, 1000.0 + 3600.0);
        let ids: Vec<_> = gone
            .iter()
            .map(|(k, o)| format!("{k}:{}", o.s("id")))
            .collect();
        assert_eq!(ids, ["item:a", "item:b", "task:t1"]);
        assert_eq!(st.arr("items").len(), 2);
        let mut st2 = obj([
            ("items", Json::Arr(vec![item("b", "answered")])),
            ("tasks", Json::Arr(vec![])),
        ]);
        assert!(prune_state(&mut st2, &HashMap::new(), 24.0, true, 0.0).is_empty());
    }

    #[test]
    fn prune_reminders_keeps_the_top_number() {
        let rem = |n: i64, state: &str| {
            obj([
                ("n", n.into()),
                ("at", "2026-01-01T00:00Z".into()),
                ("state", state.into()),
            ])
        };
        let t = obj([
            ("id", "tt-T01".into()),
            (
                "reminders",
                Json::Arr(vec![rem(1, "done"), rem(2, "done"), rem(3, "pending")]),
            ),
        ]);
        let mut st = obj([("tasks", Json::Arr(vec![t]))]);
        let now = crate::il::time::parse_ts("2026-02-01T00:00Z").unwrap() as f64;
        let gone = prune_reminders(&mut st, now);
        assert_eq!(gone.len(), 2);
        assert_eq!(gone[0].1.s("id"), "tt-T01#1");
        assert_eq!(st.arr("tasks")[0].arr("reminders").len(), 1);
    }

    #[test]
    fn reconcile_drops_orphans_only() {
        let t = tempfile::tempdir().unwrap();
        let p = Paths::at(t.path().to_path_buf());
        fs::write(&p.state, r#"{"v":5}"#).unwrap();
        let lines = [
            "{\"item\":{\"id\":\"a\"},\"cv\":5}\n",
            "{\"item\":{\"id\":\"b\"},\"cv\":6}\n",
            "{\"item\":{\"id\":\"c\"}}\n",
            "{\"item\":{\"id\":\"d\",\"cv\":9},\"cv\":4}\n",
        ];
        fs::write(&p.archive, lines.concat()).unwrap();
        reconcile_archive(&p, false).unwrap();
        assert_eq!(
            fs::read_to_string(&p.archive).unwrap(),
            [lines[0], lines[2], lines[3]].concat()
        );
        let before = fs::read(&p.archive).unwrap();
        reconcile_archive(&p, false).unwrap();
        assert_eq!(fs::read(&p.archive).unwrap(), before);
    }

    #[test]
    fn archive_lookup_skips_orphans() {
        let t = tempfile::tempdir().unwrap();
        let p = Paths::at(t.path().to_path_buf());
        fs::write(
            &p.state,
            r#"{"code":"tt","v":5,"next":{},"goals":[],"items":[],"tasks":[]}"#,
        )
        .unwrap();
        fs::write(&p.archive, "{\"item\":{\"id\":\"tt-T01\",\"n\":1},\"cv\":4}\n{\"item\":{\"id\":\"tt-T01\",\"n\":2},\"cv\":9}\n").unwrap();
        assert_eq!(
            archive_lookup(&p, "tt-T01").unwrap(),
            r#"{"item": {"id": "tt-T01", "n": 1}, "cv": 4}"#
        );
        assert_eq!(
            archive_lookup(&p, "tt-T02").unwrap_err().to_string(),
            "tt-T02 not in archive.jsonl"
        );
    }

    #[test]
    fn rotate_plan_splits_by_age() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("log.jsonl");
        fs::write(&f, "{\"t\":\"2026-01-01T00:00Z\",\"id\":\"a\",\"ev\":\"open\"}\nnot json\n{\"ts\":\"2026-03-01T00:00Z\",\"kind\":\"digest\"}\n").unwrap();
        let cutoff = crate::il::time::parse_ts("2026-02-01T00:00Z").unwrap();
        let (old, keep) = rotate_plan(&f, cutoff).unwrap().unwrap();
        assert_eq!((old.len(), keep.len()), (1, 2));
        assert!(
            rotate_plan(&t.path().join("none"), cutoff)
                .unwrap()
                .is_none()
        );
    }
}
