//! #11474: `tm architect poll --once` raises one `owner_answer` alert for an
//! answered dashboard decision, as `fleet-poll.py` does, and no duplicate on
//! an unchanged dashboard.

mod cycle_support;

use std::fs;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use trusty_architect::FakeClock;
use trusty_architect::poller::{PollSession, PollState};

/// Writes an answered decision `D97` into `<root>/local/owner-queue/items`.
fn seed_answer(env: &Env) -> TestResult {
    let dir = env.root().join("local/owner-queue/items");
    fs::create_dir_all(&dir)?;
    let doc = serde_json::json!({
        "status": "answered",
        "answer": "Option A",
        "answered_at": "2026-11-18T15:53:00Z",
        "version": 1,
    });
    fs::write(
        dir.join("D97.json"),
        serde_json::to_string_pretty(&doc)? + "\n",
    )?;
    Ok(())
}

/// The `owner_answer` rows of `alerts.jsonl`, as `kind|fingerprint`.
fn answers(env: &Env) -> TestResult<Vec<String>> {
    Ok(env
        .keys()?
        .into_iter()
        .filter(|k| k.contains("\"owner_answer\""))
        .collect())
}

#[test]
fn an_answered_decision_raises_one_owner_answer_alert_and_no_duplicate() -> TestResult {
    let env = Env::new(&[])?;
    seed_answer(&env)?;
    let tmux = fleet(&[("tm-active", "%11", "2.1.283")], &screen(10));
    let clock = FakeClock::new(1_000.0);
    let mut session = PollSession::new(PollState::default());
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    let want = ["\"\"|\"\"|\"owner_answer\"|\"D97@2026-11-18T15:53:00Z\"".to_owned()];
    assert_eq!(answers(&env)?, want, "{}", env.log());
    // A second cycle on the unchanged dashboard appends no duplicate.
    trusty_architect::Clock::sleep(&clock, std::time::Duration::from_secs(60));
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert_eq!(answers(&env)?, want, "{}", env.log());
    Ok(())
}
