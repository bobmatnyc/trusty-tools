//! The `session` op: the PM's own record inside `state.json`.
//!
//! Why: the session record (task, next, waiting_on, owed, agents, crons, refs,
//! boundary, ctx_use) is what the digest prints first and what a fresh PM reads
//! on resume. It is capped at 2048 bytes so it cannot crowd the ledger.
//! What: [`apply_session`] validates every field the op carries, merges it into a
//! copy of `state.session` (omitted fields stay), checks the size, and replaces
//! the record. Limits and messages follow `datg.py` `apply_session`.
//! Test: `session_set_merges_and_validates`, `session_caps_and_vague_owed`,
//! `refs_merge_replace_and_remove`, `session_record_size_limit`.

use super::error::{IlResult, invalid};
use super::json::{Json, obj};
use super::limits::{int_in, lim};
use super::ops::{Ctx, Ev};
use crate::pystr::py_repr;

/// Who a wait may be on.
const WHO: [&str; 5] = ["owner", "architect", "ci", "agent", "other"];
/// Owed expectations that are not exact values.
const VAGUE: [&str; 11] = [
    "latest", "main", "master", "head", "current", "tbd", "unknown", "?", "n/a", "none", "-",
];
const SESSION_MAX: usize = 2048;
const REFS_MAX: usize = 12;
/// Owed rows the digest shows as `(n/8)`.
pub const OWED_MAX: usize = 8;

fn rows<'a>(a: &'a Json, key: &str, mx: usize) -> IlResult<&'a [Json]> {
    match a.get(key) {
        Some(Json::Arr(r)) if r.len() > mx => invalid(format!(
            "{key}: {} rows is over the cap of {mx} (max {mx}); drop or merge rows",
            r.len()
        )),
        Some(Json::Arr(r)) if r.iter().all(|x| matches!(x, Json::Obj(_))) => Ok(r),
        _ => invalid(format!("{key}: a list of at most {mx} objects")),
    }
}

fn ref_key_ok(k: &str) -> bool {
    let b = k.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..].iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
        })
}

/// `_merge_refs`: same key replaces, a null value removes the key, an empty list clears all.
fn merge_refs(old: &[Json], new: &Json) -> IlResult<Json> {
    if !new.truthy() {
        return Ok(Json::Arr(Vec::new()));
    }
    let mut refs: Vec<(String, Json)> = old
        .iter()
        .map(|r| {
            (
                r.s("key").to_owned(),
                r.get("value").cloned().unwrap_or(Json::Null),
            )
        })
        .collect();
    let holder = obj([("refs", new.clone())]);
    for r in rows(&holder, "refs", REFS_MAX)? {
        let key = match r.get("key") {
            Some(Json::Str(k)) if ref_key_ok(k) => k.clone(),
            // See #11800
            _ => return invalid("the refs key must match ^[a-z0-9][a-z0-9_.-]{0,31}$"),
        };
        if r.opt("value").is_none() {
            refs.retain(|(k, _)| *k != key);
            continue;
        }
        let v: Json = lim(r.get("value"), "refs.value", 200, true, false)?.into();
        match refs.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = v,
            None => refs.push((key, v)),
        }
    }
    if refs.len() > REFS_MAX {
        return invalid(format!("refs: at most {REFS_MAX} refs"));
    }
    Ok(Json::Arr(
        refs.into_iter()
            .map(|(k, v)| obj([("key", k.into()), ("value", v)]))
            .collect(),
    ))
}

fn waiting_on(a: &Json, ctx: &Ctx) -> IlResult<Json> {
    let mut out = Vec::new();
    for r in rows(a, "waiting_on", 5)? {
        let who = match r.get("who") {
            Some(Json::Str(w)) if WHO.contains(&w.as_str()) => w.clone(),
            _ => {
                return invalid(
                    "waiting_on.who must be one of ('owner', 'architect', 'ci', 'agent', 'other')",
                );
            }
        };
        let what = lim(r.get("what"), "waiting_on.what", 120, true, false)?;
        let since = match r.get("since").filter(|s| s.truthy()) {
            Some(s) => lim(Some(s), "waiting_on.since", 20, false, false)?,
            None => lim(
                Some(&ctx.now_iso().into()),
                "waiting_on.since",
                20,
                false,
                false,
            )?,
        };
        out.push(obj([
            ("who", who.into()),
            ("what", what.into()),
            ("since", since.into()),
        ]));
    }
    Ok(Json::Arr(out))
}

fn owed(a: &Json) -> IlResult<Vec<Json>> {
    let mut out = Vec::new();
    for r in rows(a, "owed", OWED_MAX)? {
        let exp = lim(r.get("expect"), "owed.expect", 80, true, false)?;
        if VAGUE.contains(&exp.to_lowercase().as_str()) {
            return invalid(format!(
                "owed.expect must be an exact value (SHA, run id, count), not {}",
                py_repr(&exp)
            ));
        }
        out.push(obj([
            (
                "check",
                lim(r.get("check"), "owed.check", 80, true, false)?.into(),
            ),
            ("expect", exp.into()),
            (
                "ref",
                lim(r.get("ref"), "owed.ref", 80, false, false)?.into(),
            ),
        ]));
    }
    Ok(out)
}

fn simple_rows(a: &Json, key: &str, mx: usize, fields: &[(&str, usize, bool)]) -> IlResult<Json> {
    let mut out = Vec::new();
    for r in rows(a, key, mx)? {
        let mut o = Json::Obj(Vec::new());
        for (f, max, req) in fields {
            o.set(f, lim(r.get(f), &format!("{key}.{f}"), *max, *req, false)?);
        }
        out.push(o);
    }
    Ok(Json::Arr(out))
}

/// `apply_session`: merges a session-record op into `st.session`.
///
/// # Errors
/// `Invalid` with `datg.py`'s message on any violation; the state is then discarded by the caller.
pub fn apply_session(st: &mut Json, a: &Json, ctx: &mut Ctx) -> IlResult<Ev> {
    let mut ses = match st.get("session") {
        Some(s @ Json::Obj(_)) => s.clone(),
        _ => Json::Obj(Vec::new()),
    };
    for (k, mx) in [("task", 160), ("next", 200)] {
        if let Some(v) = a.opt(k) {
            ses.set(k, lim(Some(v), k, mx, true, false)?);
        }
    }
    if a.get("waiting_on").is_some() {
        ses.set("waiting_on", waiting_on(a, ctx)?);
    }
    if a.get("owed").is_some() {
        let o = owed(a)?;
        let none = o.is_empty();
        ses.set("owed", Json::Arr(o));
        ses.set("owed_none", none);
    }
    if a.get("agents").is_some() {
        ses.set(
            "agents",
            simple_rows(
                a,
                "agents",
                6,
                &[("id", 40, true), ("goal", 40, false), ("what", 80, false)],
            )?,
        );
    }
    if a.get("crons").is_some() {
        ses.set(
            "crons",
            simple_rows(
                a,
                "crons",
                4,
                &[
                    ("id", 40, true),
                    ("schedule", 40, true),
                    ("prompt", 200, true),
                ],
            )?,
        );
    }
    if let Some(new) = a.get("refs") {
        let merged = merge_refs(ses.arr("refs"), new)?;
        ses.set("refs", merged);
    }
    if a.get("boundary").is_some_and(Json::truthy) {
        ses.set("boundary_at", ctx.now_iso());
    }
    if let Some(c) = a.opt("ctx_use") {
        if !matches!(c, Json::Obj(_)) {
            return invalid("ctx_use must be an object {active, stale, ctx, keep}");
        }
        let cu = obj([
            ("at", ctx.now_iso().into()),
            (
                "active",
                int_in(c.get("active"), "ctx_use.active", 0, 100)?.into(),
            ),
            (
                "stale",
                int_in(c.get("stale"), "ctx_use.stale", 0, 100)?.into(),
            ),
            (
                "ctx",
                int_in(c.get("ctx"), "ctx_use.ctx", 0, 1_000_000_000)?.into(),
            ),
            (
                "keep",
                lim(c.get("keep"), "ctx_use.keep", 160, false, false)?.into(),
            ),
        ]);
        ses.set("ctx_use", cu);
    }
    let mut probe = ses.clone();
    probe.set("written_at", ctx.now_iso());
    probe.set("written_epoch", ctx.epoch());
    let n = probe.dump().len();
    if n > SESSION_MAX {
        return invalid(format!(
            "session record would be {n} B (> {SESSION_MAX} B limit)"
        ));
    }
    let note: String = ses.s("task").chars().take(120).collect();
    let code = st.s("code").to_owned();
    st.set("session", ses);
    Ok(super::ops::ev(&format!("{code}-session"), "update", note))
}
