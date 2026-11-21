//! `fresh`: whether the ledger was written since the last recorded change.
//!
//! Why: a hook or the poller asks "did the PM update the ledger this turn?"
//! without reading it all. Exit 0 fresh, 1 stale, 2 unreadable, one line out.
//! What: [`fresh`] compares `session.written_epoch` (exact) or `written_at`
//! (minute precision) with `--since` or the floor `stamp.json` recorded, and with
//! `--max-age`. A floor is dropped when a write landed after the stamp's `v`.
//! Named `freshness` so it never meets the scheduler's `fresh.rs`.
//! Test: `fresh_exit_codes_and_messages`, `since_accepts_iso_and_epoch`,
//! `stamp_floor_dropped_after_a_newer_write`.

use super::json::Json;
use super::store::{Paths, load_state};
use super::time::parse_ts;
use std::fs;

fn read_json_obj(path: &std::path::Path) -> Json {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| Json::parse(&t).ok())
        .filter(|j| matches!(j, Json::Obj(_)))
        .unwrap_or(Json::Obj(Vec::new()))
}

/// `_to_epoch`: an ISO minute/second string or an integer epoch, as seconds.
fn to_epoch(v: &str) -> Result<f64, String> {
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        return v.parse::<f64>().map_err(|e| e.to_string());
    }
    parse_ts(v).map(|t| t as f64).ok_or_else(|| {
        format!(
            "bad time {}; use ISO YYYY-MM-DDTHH:MMZ or epoch seconds",
            crate::pystr::py_repr(v)
        )
    })
}

fn num(j: &Json) -> Option<f64> {
    match j {
        Json::Int(n) => Some(*n as f64),
        Json::Float(f) => Some(*f),
        _ => None,
    }
}

/// `op_fresh`: `(exit code, message)`.
pub fn fresh(paths: &Paths, since: Option<&str>, max_age: Option<f64>, now: f64) -> (i32, String) {
    let st = match load_state(paths) {
        Ok(s) => s,
        Err(e) => return (2, format!("unreadable: {e}")),
    };
    let ses = st.get("session").cloned().unwrap_or(Json::Obj(Vec::new()));
    let stamp = read_json_obj(&paths.stamp);
    let mut floor = match since {
        Some(s) => match to_epoch(s) {
            Ok(f) => Some(f),
            Err(e) => return (2, format!("unreadable: {e}")),
        },
        None => stamp.get("changed_epoch").and_then(num),
    };
    if since.is_none() && stamp.i("v").is_some_and(|sv| st.i("v").unwrap_or(0) > sv) {
        floor = None; // a write landed after the Stop that recorded the change
    }
    let written_at = ses.s("written_at");
    let Some(w_at) = parse_ts(written_at) else {
        return (1, "stale: no session write recorded".to_owned());
    };
    let exact_epoch = ses.i("written_epoch");
    let (w, exact) = match exact_epoch {
        Some(e) => (e as f64, true),
        None => (w_at as f64, false),
    };
    if !exact && let Some(f) = floor {
        floor = Some(f - f.rem_euclid(60.0)); // written_at has minute precision
    }
    let info = format!("written_at={written_at}");
    if let Some(f) = floor
        && (if exact { w <= f } else { w < f })
    {
        return (
            1,
            format!("stale: state changed after the last write; {info}"),
        );
    }
    if let Some(m) = max_age
        && now - w > m * 60.0
    {
        return (1, format!("stale: write older than {m:?} min; {info}"));
    }
    (0, format!("fresh: {info}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(state: &str, stamp: Option<&str>) -> (tempfile::TempDir, Paths) {
        let t = tempfile::tempdir().unwrap();
        let p = Paths::at(t.path().to_path_buf());
        fs::write(&p.state, state).unwrap();
        if let Some(s) = stamp {
            fs::write(&p.stamp, s).unwrap();
        }
        (t, p)
    }

    const ST: &str = r#"{"code":"tt","v":4,"goals":[],"items":[],"tasks":[],"next":{},"session":{"written_at":"2026-11-20T10:00Z","written_epoch":1000}}"#;

    #[test]
    fn fresh_exit_codes_and_messages() {
        let (_t, p) = paths(ST, None);
        assert_eq!(
            fresh(&p, None, None, 1010.0),
            (0, "fresh: written_at=2026-11-20T10:00Z".into())
        );
        assert_eq!(fresh(&p, None, Some(0.1), 1100.0).0, 1);
        assert_eq!(fresh(&p, None, Some(30.0), 1100.0).0, 0);
        let (_t2, none) = paths("{", None);
        let (code, msg) = fresh(&none, None, None, 0.0);
        assert!(
            code == 2 && msg.starts_with("unreadable: state.json malformed"),
            "{msg}"
        );
        let (_t3, nowrite) = paths(
            &ST.replace("\"written_at\":\"2026-11-20T10:00Z\",", ""),
            None,
        );
        assert_eq!(
            fresh(&nowrite, None, None, 0.0),
            (1, "stale: no session write recorded".into())
        );
    }

    #[test]
    fn since_accepts_iso_and_epoch() {
        let (_t, p) = paths(ST, None);
        assert_eq!(fresh(&p, Some("1000"), None, 0.0).0, 1);
        assert_eq!(fresh(&p, Some("999"), None, 0.0).0, 0);
        assert_eq!(fresh(&p, Some("1970-01-01T00:00Z"), None, 0.0).0, 0);
        assert_eq!(fresh(&p, Some("2026-11-20T10:00Z"), None, 0.0).0, 1);
        let (c, m) = fresh(&p, Some("later"), None, 0.0);
        assert_eq!(
            (c, m.as_str()),
            (
                2,
                "unreadable: bad time 'later'; use ISO YYYY-MM-DDTHH:MMZ or epoch seconds"
            )
        );
    }

    #[test]
    fn stamp_floor_dropped_after_a_newer_write() {
        let (_t, p) = paths(ST, Some(r#"{"v":3,"changed_epoch":2000}"#));
        assert_eq!(
            fresh(&p, None, None, 0.0).0,
            0,
            "v 4 > stamp v 3 drops the floor"
        );
        let (_t2, q) = paths(ST, Some(r#"{"v":4,"changed_epoch":2000}"#));
        assert_eq!(fresh(&q, None, None, 0.0).0, 1);
    }
}
