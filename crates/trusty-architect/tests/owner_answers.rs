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

#[test]
fn an_unreadable_store_after_a_read_raises_one_error_and_keeps_the_answer_key() -> TestResult {
    let env = Env::new(&[])?;
    seed_answer(&env)?;
    let tmux = fleet(&[("tm-active", "%11", "2.1.283")], &screen(10));
    let clock = FakeClock::new(1_000.0);
    let mut session = PollSession::new(PollState::default());
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    let key = "D97@2026-11-18T15:53:00Z";
    assert_eq!(answers(&env)?.len(), 1, "{}", env.log());
    // The store turns unreadable (a torn item file) after one good read.
    fs::write(
        env.root().join("local/owner-queue/items/D97.json"),
        "{ torn",
    )?;
    trusty_architect::Clock::sleep(&clock, std::time::Duration::from_secs(60));
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    let errors: Vec<String> = env
        .keys()?
        .into_iter()
        .filter(|k| k.contains("\"owner_answer_error\""))
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}\n{}", env.log());
    // The failed read keeps the earlier answer key through dedup.
    let state = fs::read_to_string(env.state_path())?;
    assert!(state.contains(key), "answer key lost: {state}");
    // A third cycle on the still-broken store adds no second error.
    trusty_architect::Clock::sleep(&clock, std::time::Duration::from_secs(60));
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert_eq!(answers(&env)?.len(), 1, "{}", env.log());
    let again = env.keys()?;
    let n = again
        .iter()
        .filter(|k| k.contains("owner_answer_error"))
        .count();
    assert_eq!(n, 1, "{again:?}");
    Ok(())
}

/// #12029 P2e: the poller's alerts over `alert_corpus.json` are the frozen
/// `fleet-poll.py` `owner_answer_alerts` + `owner_wont_do_alerts` on the same
/// items (relayed answers, `relayed_at`, a "Relayed" note, interactive,
/// decision, canary, parked and ILIO items), in the same order.
#[test]
fn poller_owner_answers_follow_the_python_pending_rule() -> TestResult {
    use trusty_architect::dashboard::Format;
    use trusty_architect::poller::owner_answers::{AnswerStore, owner_answer_alerts};
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/owner_answers/alert_corpus.json"))?;
    let tmp = tempfile::tempdir()?;
    let items = tmp.path().join("items");
    fs::create_dir_all(&items)?;
    for (id, data) in corpus.as_object().ok_or("corpus")? {
        let mut doc = data.clone();
        doc["version"] = serde_json::json!(1);
        fs::write(items.join(format!("{id}.json")), doc.to_string())?;
    }
    let store = AnswerStore {
        root: tmp.path().to_path_buf(),
        format: Format::Dir,
        ilio_trial: false,
    };
    let got: Vec<String> = owner_answer_alerts(&store, 1_795_784_400.0, true)
        .map_err(|e| format!("{e:?}"))?
        .into_iter()
        .map(|a| format!("{}|{}|{}", a.kind, a.fingerprint, a.display))
        .collect();
    let want = [
        "owner_answer|D01@2026-11-20T09:15:00Z|[owner] D01 answered 09:15Z: Option A",
        "owner_answer|D02@2026-11-20T09:16:00Z|[owner] D02 answered 09:16Z: Option B",
        "owner_answer|D03@2026-11-20T09:17:00Z|[owner] D03 answered 09:17Z: Ship it",
        "owner_answer|D08@2026-11-20T09:22:00Z|[owner] D08 answered 09:22Z: Yes",
        "owner_answer|D09@2026-11-20T09:23:00Z|[owner] D09 answered 09:23Z: Architect Do",
        "owner_answer|D0a@2026-11-20T09:24:00Z|[owner] D0a answered 09:24Z: Architect Do (needs target resolution): via the PM",
        "owner_answer|D0b@2026-11-20T09:25:00Z|[owner] D0b cancelled 09:25Z: dropped",
        "owner_wont_do|D0e@2026-11-20T09:28:00Z|[owner] D0e won't do: out of scope",
    ];
    assert_eq!(got, want);
    Ok(())
}
