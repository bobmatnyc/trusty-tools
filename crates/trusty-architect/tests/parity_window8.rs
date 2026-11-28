//! #11826, parity window 8: the target is `fleet-poll.py` (owner ruling via
//! the Architect, 2026-11-25). One regression test per change, each red on
//! origin/main 7cdf60451d. Synthetic panes and rows only.

mod common;
mod sched_support;

use std::fs;

use common::{key, record};
use sched_support::{T0, layout};
use trusty_architect::classify::{PaneState, evaluate, parse_pane};
use trusty_architect::sched::parity;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn rule() -> String {
    "─".repeat(40)
}

/// An idle pane whose transcript ends with `last`.
fn idle_with(last: &str) -> String {
    let r = rule();
    format!("⏺ Done.\n{last}\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass\n")
}

/// The same pane mid-turn: a background agent row makes it busy.
fn busy() -> String {
    let r = rule();
    format!(
        "⏺ Working.\n\n{r}\n❯ \n{r}\n  ⏵⏵ bypass\n  ◯ x  Doing things…  3m 2s · ↓ 12.0k tokens\n"
    )
}

fn step(capture: &str, ps: &mut PaneState, now: f64) -> Vec<String> {
    evaluate(&parse_pane(capture), ps, now, "tm-x", "%1")
        .into_iter()
        .filter(|a| a.kind == "idle")
        .map(|a| a.fingerprint)
        .collect()
}

/// fleet-poll.py:418-419: one not-idle read sets `idle_since` to None, so the
/// next idle read starts a new epoch, which alerts IDLE_AFTER later.
/// Red on origin/main: the 150 s grace kept the old epoch.
#[test]
fn window8_not_idle_read_resets_the_idle_epoch() {
    let t = 1_795_408_695.0;
    let mut ps = PaneState::default();
    let screen = idle_with("");
    assert!(step(&screen, &mut ps, t).is_empty());
    assert_eq!(step(&screen, &mut ps, t + 600.0).len(), 1);
    assert!(step(&busy(), &mut ps, t + 660.0).is_empty());
    assert_eq!(ps.idle_since, None, "the not-idle read resets the epoch");
    let hash = parse_pane(&screen).screen;
    // The new epoch is the first idle read after the blip.
    assert!(step(&screen, &mut ps, t + 720.0).is_empty());
    assert!(step(&screen, &mut ps, t + 1260.0).is_empty());
    let got = step(&screen, &mut ps, t + 1320.0);
    assert_eq!(got, [format!("since-1795409415-{hash}")]);
}

/// fleet-poll.py:416-417: the alert key is `since-<epoch>-<screen>`, so a new
/// screen on an epoch that already alerted is a new alert at once.
/// Red on origin/main: the settle rule held it until the screen had been idle
/// 600 s.
#[test]
fn window8_screen_change_on_an_alerted_epoch_alerts_at_once() {
    let t = 1_795_296_612.0;
    let mut ps = PaneState::default();
    let before = idle_with("⏺ Merged #12.");
    step(&before, &mut ps, t);
    assert_eq!(step(&before, &mut ps, t + 600.0).len(), 1);
    let after = idle_with("⏺ Started the release notes.");
    let hash = parse_pane(&after).screen;
    assert_ne!(parse_pane(&before).screen, hash);
    assert_eq!(
        step(&after, &mut ps, t + 660.0),
        [format!("since-1795296612-{hash}")]
    );
}

/// The window-8 ruling: the owner kinds the scheduler does not raise yet
/// (#11881) stay out of the percentages and are counted apart, under the
/// names fleet-poll.py's records carry.
/// Red on origin/main: they were poller-only rows and failed the verdict.
#[test]
fn window8_known_absent_owner_kinds_are_counted_apart() -> TestResult {
    let dir = tempfile::tempdir()?;
    let l = layout(dir.path());
    fs::create_dir_all(dir.path().join("inbox"))?;
    let ctx = key("tm-a", "ctx", "31");
    let mut log = String::new();
    for i in 0..3 {
        let alerts = if i == 0 { vec![ctx.clone()] } else { vec![] };
        let r = record(T0 + 60 * i, alerts, vec![]);
        log += &format!("{}\n", serde_json::to_string(&r)?);
    }
    fs::write(l.shadow_log(), log)?;
    let ts = "2023-11-15T00:00:30Z";
    let row = |s: &str, k: &str| {
        format!(
            "{{\"ts\": \"{ts}\", \"session\": \"{s}\", \"pane\": \"\", \"kind\": \"{k}\", \"fingerprint\": \"D1@x\"}}\n"
        )
    };
    let mut rows = row("tm-a", "ctx").replace("D1@x", "31");
    for k in parity::KNOWN_ABSENT {
        rows += &row("", k);
    }
    rows += &row("", "owner_feedback");
    fs::write(l.alerts(), rows)?;
    fs::write(dir.path().join("inbox/triage.jsonl"), "")?;
    let rep = parity::load(&l, T0)?;
    assert_eq!(rep.alerts.poller_only, 0, "{rep:?}");
    assert_eq!(rep.alerts.matched, 1, "{rep:?}");
    assert_eq!(rep.known_absent, [2, 1, 1, 1, 1, 1]);
    let text = parity::render(&rep);
    assert!(
        text.contains(
            "known absent, out of the percentages: 7 (owner_feedback 2, owner_unpark_due 1, \
             owner_command 1, owner_interactive 1, owner_effort 1, steer 1:"
        ),
        "{text}"
    );
    assert!(text.contains("alert match: 100.0%"), "{text}");
    Ok(())
}
