//! The dry run's structured key lines (MB1595 fix round, class B).
//!
//! Why: the shadow record rebuilt `(session, kind)` from each `would alert:`
//! display, and every display that does not have the `[session]: kind:`
//! shape (selftest, owner, dashboard, token) was keyed wrong, so the parity
//! join missed real matches.
//! What: [`alert_key_line`] is `would alert key: {"session","kind",
//! "fingerprint"}`; [`wake_key_lines`] is one `would wake key:` line per
//! woken alert with `haiku` true when no triage rule decided it. The run
//! prints `sched::shadow::KEYS_MARKER` first; `sched::shadow::parse_decision`
//! reads these lines.
//! Test: `dry_run_prints_structured_alert_and_wake_keys`.

use serde_json::{Value, json};

use super::triage::alert_key;
use crate::classify::Alert;

fn key_json(a: &Alert) -> Value {
    json!({"session": a.session, "kind": a.kind, "fingerprint": a.fingerprint})
}

/// The `would alert key:` line for `a`, indented like the run's other
/// dry-run lines.
pub fn alert_key_line(a: &Alert) -> String {
    format!("  would alert key: {}", key_json(a))
}

/// One `would wake key:` line per `woken` alert; `haiku_keys` holds the
/// [`alert_key`]s of the Haiku-path ones.
pub fn wake_key_lines(woken: &[Alert], haiku_keys: &[String]) -> Vec<String> {
    woken
        .iter()
        .map(|a| {
            let mut v = key_json(a);
            v["haiku"] = haiku_keys.contains(&alert_key(a)).into();
            format!("  would wake key: {v}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::alert;
    use crate::sched::shadow::{KEYS_MARKER, ShadowAlert, parse_decision};

    #[test]
    fn dry_run_prints_structured_alert_and_wake_keys() {
        let mut sf = alert("selftest_fail", "a,b", "a, b", "", "", 0.0);
        sf.display = "[runtime selftest]: failing: a, b".to_owned();
        let pm = alert("pm_message", "915b9b", "done: x", "mcp:tm-a", "", 0.0);
        let mut out = vec![format!("  {KEYS_MARKER}")];
        for a in [&sf, &pm] {
            out.push(format!("  would alert: {}", a.display));
            out.push(alert_key_line(a));
        }
        out.push("x panes=3 new=2 woken=2 logged=0 suppressed=0 wake=none".to_owned());
        out.extend(wake_key_lines(&[sf.clone(), pm.clone()], &[alert_key(&sf)]));
        let d = parse_decision(&out.join("\n"));
        let want = |s: &str, k: &str| ShadowAlert {
            session: s.to_owned(),
            kind: k.to_owned(),
        };
        assert_eq!(
            d.alerts,
            [want("", "selftest_fail"), want("mcp:tm-a", "pm_message")]
        );
        let keys = d.keys.unwrap_or_default();
        assert_eq!(keys[1].fingerprint, "915b9b");
        let woken = d.woken_keys.unwrap_or_default();
        assert_eq!(
            woken.iter().map(|w| w.haiku).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(woken[1].key, keys[1]);
    }
}
