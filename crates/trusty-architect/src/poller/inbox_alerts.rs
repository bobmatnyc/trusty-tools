//! Alerts other Architect tools leave in the inbox (df-T7i P1b).
//!
//! Why: `ctx-cycle.py` queues its failures in `inbox/ctx-cycle-alerts.jsonl`,
//! and `runtime_selftest.py` writes `inbox/selftest-state.json`; the
//! prototype poller turns both into alerts (ar-T5q).
//! What: [`ctx_cycle_alerts`] reads the new lines of the queue from the byte
//! offset in `state["ctx_cycle_offset"]` (not advanced in a dry run), one
//! `ctxcycle` alert per JSON object line; [`selftest_alerts`] raises one
//! `selftest_fail` alert while the state file lists failing checks,
//! fingerprinted by the failing set. A missing or unreadable file is no
//! alerts, as in the prototype; a line or file of the wrong shape is skipped
//! (the prototype raises `AttributeError` and loses the cycle).
//! Test: `inbox_alerts_match_python_goldens` (tests/parity_p1b.rs),
//! `ctx_cycle_offset_holds_in_a_dry_run_and_skips_bad_lines`,
//! `selftest_state_of_the_wrong_shape_is_no_alert`.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::classify::{Alert, alert};
use crate::pystr::splitlines;

/// A JSON value as the prototype's f-string or `alert()` shows it.
fn text_of(v: Option<&Value>) -> String {
    match v {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// `ctx_cycle_alerts(state, now, dry_run, path)`.
pub fn ctx_cycle_alerts(
    other: &mut Map<String, Value>,
    path: &Path,
    now: f64,
    dry_run: bool,
) -> Vec<Alert> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let offset = other
        .get("ctx_cycle_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let data = String::from_utf8_lossy(&bytes[start..]);
    let out = splitlines(&data)
        .into_iter()
        .filter_map(|line| serde_json::from_str::<Map<String, Value>>(line).ok())
        .map(|d| {
            alert(
                "ctxcycle",
                &text_of(d.get("fingerprint")),
                &text_of(d.get("detail")),
                &text_of(d.get("session")),
                "",
                now,
            )
        })
        .collect();
    if !dry_run {
        // Python's tell() after a seek past the end stays at the seek.
        let end = offset.max(bytes.len() as u64);
        other.insert("ctx_cycle_offset".to_owned(), end.into());
    }
    out
}

/// `selftest_alerts(now, inbox)`: one `selftest_fail` alert while
/// `selftest-state.json` lists failures.
pub fn selftest_alerts(path: &Path, now: f64) -> Vec<Alert> {
    let Some(st) = fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Map<String, Value>>(&t).ok())
    else {
        return Vec::new();
    };
    let strings = |key: &str| -> Vec<String> {
        st.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let fails = strings("failures");
    if fails.is_empty() {
        return Vec::new();
    }
    let lines = strings("lines");
    let detail = if lines.is_empty() { &fails } else { &lines }.join("; ");
    let mut a = alert("selftest_fail", &fails.join(","), &detail, "", "", now);
    a.display = format!("[runtime selftest]: failing: {detail}");
    vec![a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_cycle_offset_holds_in_a_dry_run_and_skips_bad_lines() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("ctx-cycle-alerts.jsonl");
        fs::write(
            &path,
            "[1]\nnot json\n{\"session\": \"tm-a\", \"fingerprint\": \"f\", \"detail\": \"d\"}\n",
        )?;
        let mut other = Map::new();
        let found = ctx_cycle_alerts(&mut other, &path, 0.0, true);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].session.as_str(), found[0].fingerprint.as_str()),
            ("tm-a", "f")
        );
        assert!(other.is_empty(), "a dry run keeps the offset");
        ctx_cycle_alerts(&mut other, &path, 0.0, false);
        assert!(ctx_cycle_alerts(&mut other, &path, 0.0, false).is_empty());
        let before = other.clone();
        assert!(ctx_cycle_alerts(&mut other, &dir.path().join("missing"), 0.0, false).is_empty());
        assert_eq!(other, before, "a missing file keeps the offset");
        Ok(())
    }

    #[test]
    fn selftest_state_of_the_wrong_shape_is_no_alert() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("selftest-state.json");
        assert!(selftest_alerts(&path, 0.0).is_empty(), "missing file");
        for text in ["[\"a\"]", "{\"failures\": []}", "not json"] {
            fs::write(&path, text)?;
            assert!(selftest_alerts(&path, 0.0).is_empty(), "{text}");
        }
        Ok(())
    }
}
