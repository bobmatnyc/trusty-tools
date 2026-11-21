//! The shape a `state.json` must have, checked before every write.
//!
//! Why: no schema file exists for the live format; `datg.py` checks only the
//! top-level shape on load and trusts its own ops. The port validates the whole
//! document on load and again just before the commit, so a bad op or a damaged
//! file is refused with nothing written. The rules are those the scripts
//! enforce, derived from `datg.py` and pinned by the goldens.
//! What: [`validate`] returns the first violation as a message naming the path.
//! It is deliberately no stricter than the live ledgers: optional keys may be
//! absent, unknown keys are kept, and only types, enums and id rules are checked.
//! Test: `live_shaped_state_validates`, `violations_name_the_path`.

use super::error::{IlError, IlResult};
use super::ids::{dec, is_code, parse_id};
use super::json::Json;
use std::collections::HashSet;

const GOAL: &[&str] = &["active", "done", "parked"];
const ITEM: &[&str] = &["open", "answered", "done"];
const TASK: &[&str] = &["open", "running", "done", "parked"];
const REMINDER: &[&str] = &["pending", "fired", "done"];

fn bad<T>(path: &str, what: &str) -> IlResult<T> {
    Err(IlError::Corrupt(format!(
        "state.json schema: {path}: {what}; refusing to write"
    )))
}

fn need_str(o: &Json, k: &str, at: &str) -> IlResult<()> {
    match o.get(k) {
        Some(Json::Str(_)) => Ok(()),
        _ => bad(&format!("{at}.{k}"), "expected a string"),
    }
}

fn want_int(o: &Json, k: &str, at: &str, lo: i64, hi: i64) -> IlResult<()> {
    match o.get(k) {
        None => Ok(()),
        Some(Json::Int(n)) if (lo..=hi).contains(n) => Ok(()),
        _ => bad(
            &format!("{at}.{k}"),
            &format!("expected an integer {lo}-{hi}"),
        ),
    }
}

fn entity(
    o: &Json,
    at: &str,
    code: &str,
    kinds: &str,
    allowed: &[&str],
    seen: &mut HashSet<String>,
) -> IlResult<()> {
    if !matches!(o, Json::Obj(_)) {
        return bad(at, "expected an object");
    }
    need_str(o, "id", at)?;
    let id = o.s("id");
    match parse_id(id) {
        Some((c, k, _)) if c == code && kinds.contains(k) => {}
        _ => {
            return bad(
                &format!("{at}.id"),
                &format!("{id:?} is not a {code}-<{kinds}><2 base-34 digits> id"),
            );
        }
    }
    if !seen.insert(id.to_owned()) {
        return bad(&format!("{at}.id"), &format!("{id:?} appears twice"));
    }
    match o.get("status") {
        Some(Json::Str(s)) if allowed.contains(&s.as_str()) => {}
        _ => {
            return bad(
                &format!("{at}.status"),
                &format!("expected one of {allowed:?}"),
            );
        }
    }
    need_str(o, "title", at)
}

fn reminders(t: &Json, at: &str) -> IlResult<()> {
    match t.get("reminders") {
        None | Some(Json::Null) => return Ok(()),
        Some(Json::Arr(_)) => {}
        _ => return bad(&format!("{at}.reminders"), "expected an array"),
    }
    for (i, r) in t.arr("reminders").iter().enumerate() {
        let at = format!("{at}.reminders[{i}]");
        if !matches!(r.get("n"), Some(Json::Int(_))) {
            return bad(&format!("{at}.n"), "expected an integer");
        }
        need_str(r, "at", &at)?;
        match r.get("state") {
            Some(Json::Str(s)) if REMINDER.contains(&s.as_str()) => {}
            _ => return bad(&format!("{at}.state"), "expected pending, fired or done"),
        }
    }
    Ok(())
}

/// Checks `st` against the ledger shape.
///
/// # Errors
/// `Corrupt` naming the first path that breaks a rule.
pub fn validate(st: &Json) -> IlResult<()> {
    if !matches!(st, Json::Obj(_)) {
        return bad("$", "expected an object");
    }
    let code = st.s("code");
    if !is_code(code) {
        return bad(
            "code",
            "expected 2-8 chars, lowercase letters or digits, letter first",
        );
    }
    if !matches!(st.get("v"), Some(Json::Int(n)) if *n >= 0) {
        return bad("v", "expected a non-negative integer");
    }
    for k in ["config", "session"] {
        if !matches!(st.get(k), None | Some(Json::Obj(_))) {
            return bad(k, "expected an object");
        }
    }
    match st.get("next") {
        Some(Json::Obj(m)) => {
            for (k, v) in m {
                if !matches!(v, Json::Str(s) if dec(s).is_some()) {
                    return bad(&format!("next.{k}"), "expected two base-34 digits");
                }
            }
        }
        _ => return bad("next", "expected an object"),
    }
    let mut seen = HashSet::new();
    for (key, kinds, allowed) in [
        ("goals", "G", GOAL),
        ("items", "DA", ITEM),
        ("tasks", "T", TASK),
    ] {
        if !matches!(st.get(key), Some(Json::Arr(_))) {
            return bad(key, "expected an array");
        }
        for (i, o) in st.arr(key).iter().enumerate() {
            let at = format!("{key}[{i}]");
            entity(o, &at, code, kinds, allowed, &mut seen)?;
            match key {
                "goals" => {
                    if !matches!(o.get("rank"), Some(Json::Int(_))) {
                        return bad(&format!("{at}.rank"), "expected an integer");
                    }
                }
                "tasks" => {
                    need_str(o, "goal", &at)?;
                    want_int(o, "priority", &at, 1, 5)?;
                    want_int(o, "pct", &at, 0, 100)?;
                    reminders(o, &at)?;
                }
                _ => need_str(o, "goal", &at)?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"{"code":"tt","v":3,"config":{"block_after":"x"},"goals":[{"id":"tt-G01","rank":1,"title":"g","finish":"","status":"active","maps":""}],"items":[{"id":"tt-D01","kind":"question","status":"open","goal":"tt-G01","title":"q","opts":[],"answer":"","outcome":"","esc":null}],"tasks":[{"id":"tt-T01","goal":"tt-G01","title":"t","status":"open","to":"","outcome":"","priority":2,"reminders":[{"n":1,"at":"2026-01-01T00:00Z","every":null,"note":"n","state":"pending"}]}],"next":{"G":"02","D":"02","A":"01","T":"02"},"session":{}}"#;

    #[test]
    fn live_shaped_state_validates() {
        validate(&Json::parse(OK).unwrap()).unwrap();
    }

    #[test]
    fn violations_name_the_path() {
        let cases = [
            (
                OK.replace("\"status\":\"open\",\"to\"", "\"status\":\"weird\",\"to\""),
                "tasks[0].status",
            ),
            (OK.replace("tt-T01", "tt-D01"), "tasks[0].id"),
            (
                OK.replace("\"priority\":2", "\"priority\":9"),
                "tasks[0].priority",
            ),
            (OK.replace("\"G\":\"02\"", "\"G\":\"0o\""), "next.G"),
            (OK.replace("\"v\":3", "\"v\":-1"), "v"),
        ];
        for (text, path) in cases {
            let e = validate(&Json::parse(&text).unwrap())
                .unwrap_err()
                .to_string();
            assert!(e.contains(path), "{path}: {e}");
        }
    }
}
