//! MB1595 gap 2: `alert_triage.py`'s rules decide which new alerts wake. The
//! live poller logs a `ctx` alert below the 35% bar (`rule:ctx_below_bar`);
//! the Rust poller woke for every alert. Every file is in a tempdir.

use std::fs;

use trusty_architect::classify::alert;
use trusty_architect::poller::{PollState, RuleTriage, Triage};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn triage(dir: &std::path::Path, holds: &str) -> std::io::Result<RuleTriage> {
    let rules = dir.join("triage-rules.json");
    fs::write(&rules, format!(r#"{{"holds": {holds}}}"#))?;
    Ok(RuleTriage {
        rules,
        schedule: dir.join("il-schedule.json"),
    })
}

fn kinds(a: &[trusty_architect::classify::Alert]) -> Vec<(&str, &str)> {
    a.iter()
        .map(|a| (a.kind.as_str(), a.session.as_str()))
        .collect()
}

#[test]
fn ctx_below_the_bar_is_logged_and_at_the_bar_wakes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = triage(dir.path(), "[]")?;
    let new = [
        alert("ctx", "31", "ctx 32%", "tm-personal", "%1", 0.0),
        alert("ctx", "35", "ctx 36%", "tm-b", "%2", 0.0),
    ];
    let (woken, logged) = t.triage(&new, 0.0, &mut PollState::default(), true);
    assert_eq!((kinds(&woken), logged), (vec![("ctx", "tm-b")], 1));
    Ok(())
}

#[test]
fn held_idle_is_logged_and_unruled_idle_wakes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let holds = r#"[{"session": "held", "reason": "monitor only", "until": "2099-01-01T00:00:00Z"},
                   {"session": "old", "reason": "expired", "until": "2000-01-01T00:00:00Z"}]"#;
    let t = triage(dir.path(), holds)?;
    let new = [
        alert("idle", "f", "idle 11 min", "tm-held", "%1", 0.0),
        alert("idle", "f", "idle 11 min", "tm-old", "%2", 0.0),
        alert("dialog", "f", "Allow?", "tm-held", "%1", 0.0),
    ];
    let (woken, logged) = t.triage(&new, 1.0e9, &mut PollState::default(), true);
    // An unruled idle goes to the Haiku gate, which fails open to a wake.
    assert_eq!(
        (kinds(&woken), logged),
        (vec![("idle", "tm-old"), ("dialog", "tm-held")], 1)
    );
    Ok(())
}

#[test]
fn rc_failed_wakes_on_the_third_in_a_row() -> TestResult {
    let dir = tempfile::tempdir()?;
    let t = triage(dir.path(), "[]")?;
    let mut st = PollState::default();
    let one = [alert("rc_failed", "f", "rc failed", "tm-a", "%1", 0.0)];
    let mut woke = Vec::new();
    for _ in 0..3 {
        woke.push(t.triage(&one, 0.0, &mut st, true).0.len());
    }
    // An unruled rc_failed before the third also fails open to a wake.
    assert_eq!(woke, [1, 1, 1]);
    assert_eq!(st.other["triage_rc"]["tm-a"], 3);
    let restored = [alert("rc_restored", "f", "back", "tm-a", "%1", 0.0)];
    assert_eq!(t.triage(&restored, 0.0, &mut st, true), (vec![], 1));
    assert!(st.other["triage_rc"].get("tm-a").is_none());
    Ok(())
}
