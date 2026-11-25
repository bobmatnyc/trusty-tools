//! #11826, parity window 7 (2026-11-25T08:15Z on): the owner rows the live
//! poller raised and the scheduler did not, as fixtures from the window's
//! own rows (`trusty-architect-sched parity --by-kind --examples`): item ids,
//! times and notes as the owner-queue store held them when the poller read.
//!
//! `owner_wont_do` had no step in the Rust cycle; each test for it is red
//! on the pre-fix commit. `owner_answer` raised the same
//! fingerprints from the same store; its window-7 misses were the relay race
//! (the live wake made the Architect relay the answer before the shadow's
//! next read), shown by `window7_owner_answer_rows_match_the_poller_on_its_store`.

mod cycle_support;

use std::fs;
use std::time::Duration;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use serde_json::{Value, json};
use trusty_architect::poller::{PollSession, PollState};
use trusty_architect::{Clock, FakeClock, ScriptedTmux};

/// Writes `doc` as `<root>/local/owner-queue/<coll>/<id>.json` (dir format).
fn put(env: &Env, coll: &str, id: &str, doc: &Value) -> TestResult {
    let dir = env.root().join("local/owner-queue").join(coll);
    fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(doc)? + "\n";
    fs::write(dir.join(format!("{id}.json")), text)?;
    Ok(())
}

/// The fingerprints of `kind` in `alerts.jsonl`, in file order.
fn fps(env: &Env, kind: &str) -> TestResult<Vec<String>> {
    let want = format!("|\"{kind}\"|");
    Ok(env
        .keys()?
        .into_iter()
        .filter(|k| k.contains(&want))
        .map(|k| {
            k.rsplit('|')
                .next()
                .unwrap_or_default()
                .trim_matches('"')
                .to_owned()
        })
        .collect())
}

/// The `display` of each `kind` row in `alerts.jsonl`, in file order.
fn displays(env: &Env, kind: &str) -> TestResult<Vec<String>> {
    let text = fs::read_to_string(env.root().join("inbox/alerts.jsonl"))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let v: Value = serde_json::from_str(line)?;
        if v["kind"] == kind {
            out.push(v["display"].as_str().unwrap_or_default().to_owned());
        }
    }
    Ok(out)
}

/// One fleet, one clock, one session: runs `n` cycles a minute apart.
struct Cycles {
    tmux: ScriptedTmux,
    clock: FakeClock,
    session: PollSession,
}

impl Cycles {
    fn new() -> Self {
        Self {
            tmux: fleet(&[("tm-active", "%11", "2.1.283")], &screen(10)),
            clock: FakeClock::new(1_000.0),
            session: PollSession::new(PollState::default()),
        }
    }

    fn run(&mut self, env: &Env) -> TestResult {
        env.runner(&self.tmux, &self.clock, &QuietHost)
            .run_once(&mut self.session)?;
        self.clock.sleep(Duration::from_secs(60));
        Ok(())
    }
}

fn wont_do(at: &str, note: &str) -> Value {
    json!({
        "kind": "blocker",
        "status": "wont_do",
        "wont_do": {"at": at, "attempts": 1, "by": "Bob", "note": note},
        "version": 2,
    })
}

/// Window 7's poller-only `owner_wont_do` rows (Bny, Bp2, Bp4, Bp0), each as
/// the store held it before the Architect relayed or acted on it. Red before
/// the fix: the cycle had no won't-do step.
#[test]
fn window7_owner_wont_do_rows_raise_on_the_scheduler() -> TestResult {
    let env = Env::new(&[])?;
    let rows = [
        ("Bny", "2026-11-25T05:27:32.685Z", "You can do this better."),
        ("Bp0", "2026-11-25T09:43:23.278Z", "Archirect do"),
        (
            "Bp2",
            "2026-11-25T07:59:42.413Z",
            "Accept risk.  Scrub transcript.",
        ),
        ("Bp4", "2026-11-25T09:40:42.298Z", "architect do"),
    ];
    for (id, at, note) in rows {
        put(&env, "items", id, &wont_do(at, note))?;
    }
    let mut c = Cycles::new();
    c.run(&env)?;
    let want: Vec<String> = rows
        .iter()
        .map(|(id, at, _)| format!("{id}@{at}"))
        .collect();
    assert_eq!(fps(&env, "owner_wont_do")?, want, "{}", env.log());
    let shown = displays(&env, "owner_wont_do")?;
    assert!(
        shown.contains(&"[owner] Bp2 won't do: Accept risk. Scrub transcript.".to_owned()),
        "{shown:?}"
    );
    // An unchanged store raises no duplicate; a relayed item stops.
    let mut relayed = wont_do(rows[2].1, rows[2].2);
    relayed["relayed"] = json!("2026-11-25T08:04Z scrubbed");
    put(&env, "items", "Bp2", &relayed)?;
    c.run(&env)?;
    assert_eq!(fps(&env, "owner_wont_do")?, want, "{}", env.log());
    Ok(())
}

/// A failed item read keeps the won't-do keys, so the next good read does
/// not raise the same won't-do again. Red before the fix (no won't-do key).
#[test]
fn window7_failed_item_read_keeps_the_wont_do_key() -> TestResult {
    let env = Env::new(&[])?;
    let (at, note) = ("2026-11-25T07:59:42.413Z", "Accept risk.");
    put(&env, "items", "Bp2", &wont_do(at, note))?;
    let mut c = Cycles::new();
    c.run(&env)?;
    assert_eq!(fps(&env, "owner_wont_do")?.len(), 1, "{}", env.log());
    fs::write(
        env.root().join("local/owner-queue/items/Bp2.json"),
        "{ torn",
    )?;
    c.run(&env)?;
    assert_eq!(fps(&env, "owner_answer_error")?, ["StoreError"]);
    let state = fs::read_to_string(env.state_path())?;
    assert!(
        state.contains(&format!("||owner_wont_do|Bp2@{at}")),
        "{state}"
    );
    put(&env, "items", "Bp2", &wont_do(at, note))?;
    c.run(&env)?;
    assert_eq!(fps(&env, "owner_wont_do")?.len(), 1, "{}", env.log());
    Ok(())
}

fn answered(at: &str, answer: &str) -> Value {
    json!({"kind": "decision", "status": "answered", "answer": answer,
           "answer_note": "", "answered_at": at, "version": 2})
}

/// Window 7's poller-only `owner_answer` rows (D0i, Dna at 08:21Z; Daz, Dmy,
/// Dmz, Dn4 at 03:39Z): on the store the poller read, the scheduler raises
/// the same fingerprints. Once the Architect has relayed them (08:22:58Z and
/// 03:39:44Z, `inbox/relays.jsonl`), it raises none: the shadow cycles that
/// followed (08:22:42-08:23:52Z, 03:39:36-03:40:23Z, `local/meta/runs.jsonl`)
/// read after the relay. Passes on the pre-fix commit too: no scheduler bug.
#[test]
fn window7_owner_answer_rows_match_the_poller_on_its_store() -> TestResult {
    let env = Env::new(&[])?;
    let rows = [
        ("D0i", "2026-11-25T08:22:02.210Z"),
        ("Daz", "2026-11-25T03:39:09Z"),
        ("Dmy", "2026-11-25T03:39:09Z"),
        ("Dmz", "2026-11-25T03:39:09Z"),
        ("Dn4", "2026-11-25T03:39:09Z"),
        ("Dna", "2026-11-25T08:21:52.778Z"),
    ];
    for (id, at) in rows {
        put(&env, "items", id, &answered(at, "Option A"))?;
    }
    let mut c = Cycles::new();
    c.run(&env)?;
    let want: Vec<String> = rows.iter().map(|(id, at)| format!("{id}@{at}")).collect();
    assert_eq!(fps(&env, "owner_answer")?, want, "{}", env.log());
    // Relayed before the next read: nothing more.
    let fresh = Env::new(&[])?;
    for (id, at) in rows {
        let mut doc = answered(at, "Option A");
        doc["relayed"] = json!("2026-11-25T08:23Z tm-ta: relayed");
        put(&fresh, "items", id, &doc)?;
    }
    Cycles::new().run(&fresh)?;
    assert!(fps(&fresh, "owner_answer")?.is_empty(), "{}", fresh.log());
    Ok(())
}

/// An answered item of the ILIO trial (`refs` holds `arch:ilio`) is the
/// trial's to relay; the live Architect's poller raises nothing for it
/// (`owner_answers.pending`, ar-T2f). Red before the fix.
#[test]
fn window7_ilio_items_raise_no_owner_alert_for_the_live_architect() -> TestResult {
    let env = Env::new(&[])?;
    let mut ilio = answered("2026-11-25T10:00:00Z", "Option B");
    ilio["refs"] = json!(["arch:ilio"]);
    put(&env, "items", "Dnj", &ilio)?;
    let mut ilio_wd = wont_do("2026-11-25T10:01:00Z", "no");
    ilio_wd["refs"] = json!(["arch:ilio", "other"]);
    put(&env, "items", "Dnk", &ilio_wd)?;
    put(
        &env,
        "items",
        "D0i",
        &answered("2026-11-25T08:22:02.210Z", "C"),
    )?;
    Cycles::new().run(&env)?;
    assert_eq!(fps(&env, "owner_answer")?, ["D0i@2026-11-25T08:22:02.210Z"]);
    assert!(fps(&env, "owner_wont_do")?.is_empty(), "{}", env.log());
    Ok(())
}
