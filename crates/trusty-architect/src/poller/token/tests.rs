//! The deployed `scripts/test_token_watch.py` cases, ported by name, on
//! synthetic transcripts. All are new with the port (MB1595 fix round): the
//! base had no `token` step, so none of them could compile there.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};

use super::clock::{hour_fp, hour_of};
use super::scan::TRIAGE_PREFIX;
use super::state::{FleetHour, OpenHour, TokenState, rollup_row, save};
use super::units::{effort_of, family, thinking_of, turn_units};
use super::*;

const NOW: f64 = 1_791_376_200.0; // 2026-10-07T12:30:00Z
const OPUS: &str = "claude-opus-5";
const SONNET: &str = "claude-sonnet-5";

fn iso_ms(t: f64) -> String {
    format!(
        "{}.000Z",
        crate::classify::alerts::iso(t).trim_end_matches('Z')
    )
}

#[derive(Clone, Copy)]
struct L<'a> {
    model: &'a str,
    read: u64,
    create: u64,
    inp: u64,
    out: u64,
    cmd: Option<&'a str>,
}

const D: L<'static> = L {
    model: OPUS,
    read: 0,
    create: 0,
    inp: 0,
    out: 10,
    cmd: None,
};

fn line_with(mid: &str, t: f64, l: L<'_>) -> String {
    let content = match l.cmd {
        Some(c) => json!([{"type": "tool_use", "name": "Bash", "input": {"command": c}}]),
        None => json!([{"type": "text", "text": "x"}]),
    };
    let d = json!({"type": "assistant", "timestamp": iso_ms(t), "message": {
        "id": mid, "model": l.model, "content": content,
        "usage": {"input_tokens": l.inp, "cache_read_input_tokens": l.read,
                  "cache_creation_input_tokens": l.create, "output_tokens": l.out}}});
    format!("{d}\n")
}

fn line(mid: &str, t: f64, read: u64) -> String {
    line_with(mid, t, L { read, ..D })
}

struct Env {
    tmp: PathBuf,
    base: PathBuf,
}

impl Env {
    fn new(tmp: &Path) -> Self {
        fs::create_dir_all(tmp).expect("mkdir");
        Self {
            tmp: tmp.to_owned(),
            base: tmp.join("projects"),
        }
    }
    fn main_in(&self, sid: &str, proj: &str) -> PathBuf {
        let p = self.base.join(proj).join(format!("{sid}.jsonl"));
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        p
    }
    fn main(&self) -> PathBuf {
        self.main_in("aaaaaaaa-0000", "-Users-x-proj")
    }
    fn sub(&self, aid: &str) -> PathBuf {
        let dir = self.base.join("-Users-x-proj/aaaaaaaa-0000/subagents");
        fs::create_dir_all(&dir).expect("mkdir");
        let meta = json!({"agentType": "research"}).to_string();
        fs::write(dir.join(format!("agent-{aid}.meta.json")), meta).expect("meta");
        touch(&dir);
        dir.join(format!("agent-{aid}.jsonl"))
    }
    fn write(&self, p: &Path, lines: &[String]) {
        fs::write(p, lines.concat()).expect("write");
        touch(p);
    }
    fn append(&self, p: &Path, lines: &[String]) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(p).expect("open");
        f.write_all(lines.concat().as_bytes()).expect("append");
        drop(f);
        touch(p);
    }
    fn mode(&self, dry: bool) -> Mode {
        Mode {
            state: self.tmp.join("state.json"),
            seed: None,
            save: !dry,
            hourly: (!dry).then(|| self.tmp.join("hourly.jsonl")),
        }
    }
    fn run_at(&self, now: f64, dry: bool) -> Vec<TokenAlert> {
        let paths = Paths {
            base: &self.base,
            sessions: &self.tmp,
            thresholds: self.tmp.join("none.json"),
            schedule: &self.tmp,
        };
        step(now, &self.mode(dry), &paths, Some(Default::default())).expect("step")
    }
    fn run(&self) -> Vec<TokenAlert> {
        self.run_at(NOW, false)
    }
    fn state(&self) -> TokenState {
        state::load(&self.tmp.join("state.json")).expect("state")
    }
    fn set_state(&self, st: &TokenState) {
        save(&self.tmp.join("state.json"), st).expect("save");
    }
}

fn touch(p: &Path) {
    let t = UNIX_EPOCH + Duration::from_secs_f64(NOW);
    let f = fs::File::options().read(true).open(p).expect("open");
    f.set_modified(t).expect("mtime");
}

fn fps(a: &[TokenAlert]) -> Vec<String> {
    let mut v: Vec<String> = a.iter().map(|a| a.fingerprint.clone()).collect();
    v.sort();
    v
}

fn rules(a: &[TokenAlert]) -> Vec<&str> {
    a.iter().map(|a| a.rule.as_str()).collect()
}

fn opens(st: &TokenState) -> Vec<&OpenHour> {
    st.open.values().collect()
}

#[test]
fn ctx_main_buckets_once() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let ls: Vec<String> = [190_000, 205_000, 230_000, 320_000]
        .iter()
        .enumerate()
        .map(|(i, c)| line(&format!("m{i}"), NOW - 600.0 + i as f64, *c))
        .collect();
    e.write(&e.main(), &ls);
    assert_eq!(fps(&e.run()), ["ctx_main:200k", "ctx_main:315k"]);
    assert!(e.run().is_empty());
    e.append(&e.main(), &[line("m9", NOW - 10.0, 190_000)]);
    assert!(e.run().is_empty());
}

#[test]
fn ctx_sub_uses_subagent_buckets() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    e.write(&e.sub("bbbbbbbb1"), &[line("s1", NOW - 60.0, 160_000)]);
    e.write(&e.main(), &[line("m1", NOW - 60.0, 160_000)]);
    assert_eq!(fps(&e.run()), ["ctx_sub:150k"]);
}

#[test]
fn miss_idle_vs_unexpected() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let a1 = line_with(
        "a1",
        NOW - 900.0,
        L {
            read: 10_000,
            out: 5,
            ..D
        },
    );
    let rw = |mid, t| {
        line_with(
            mid,
            t,
            L {
                read: 16_000,
                create: 160_000,
                ..D
            },
        )
    };
    e.write(&e.sub("idleidle1"), &[a1.clone(), rw("a2", NOW - 480.0)]);
    let b1 = line_with(
        "b1",
        NOW - 900.0,
        L {
            read: 10_000,
            out: 5,
            ..D
        },
    );
    e.write(&e.sub("quickqui1"), &[b1, rw("b2", NOW - 780.0)]);
    let got = fps(&e.run());
    assert!(got.contains(&"miss_idle:idleidle".to_owned()), "{got:?}");
    assert!(got.contains(&"miss_unexpected:b2".to_owned()), "{got:?}");
    assert!(!got.contains(&"miss_idle:quickqui".to_owned()));
    assert!(!got.contains(&"miss_unexpected:a2".to_owned()));
    let st = e.state();
    assert_eq!(opens(&st).iter().map(|o| o.rw_exp).sum::<u64>(), 1);
    assert_eq!(opens(&st).iter().map(|o| o.rw_unexp).sum::<u64>(), 1);
}

#[test]
fn miss_unexpected_burst_is_capped() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let mut ls = vec![line("c0", NOW - 900.0, 10_000)];
    for i in 1..6 {
        let t = NOW - 900.0 + 20.0 * f64::from(i);
        ls.push(line_with(
            &format!("c{i}"),
            t,
            L {
                read: 10_000,
                create: 150_000,
                ..D
            },
        ));
    }
    e.write(&e.main(), &ls);
    let got = fps(&e.run());
    let singles = got
        .iter()
        .filter(|g| g.starts_with("miss_unexpected:c"))
        .count();
    assert_eq!(singles, 3);
    let bursts: Vec<&String> = got.iter().filter(|g| g.contains("burst")).collect();
    assert_eq!(
        bursts,
        [&format!("miss_unexpected:burst:{}", hour_fp(NOW - 800.0))]
    );
}

#[test]
fn poll_agent_fires_at_twelve() {
    let mk = |n: usize| -> Vec<String> {
        (0..n)
            .map(|i| {
                let l = L {
                    model: SONNET,
                    read: 1000,
                    cmd: Some("tm wait --for run x"),
                    ..D
                };
                line_with(&format!("p{i}"), NOW - 1500.0 + 60.0 * i as f64, l)
            })
            .collect()
    };
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    e.write(&e.sub("pollpoll1"), &mk(11));
    assert!(e.run().is_empty());
    let e2 = Env::new(&d.path().join("again"));
    e2.write(&e2.sub("pollpoll1"), &mk(12));
    assert_eq!(fps(&e2.run()), ["poll_agent:pollpoll"]);
    assert!(e2.run().is_empty());
}

#[test]
fn hit_low() {
    let mk = |r: u64| -> Vec<String> {
        (0..20)
            .map(|i| {
                let l = L {
                    read: r,
                    inp: 100_000 - r,
                    ..D
                };
                line_with(&format!("h{i}"), NOW - 1000.0 + f64::from(i), l)
            })
            .collect()
    };
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    e.write(&e.main(), &mk(98_000));
    assert!(e.run().is_empty());
    let e2 = Env::new(&d.path().join("low"));
    e2.write(&e2.main(), &mk(80_000));
    let got = e2.run();
    assert_eq!(
        fps(&got),
        [format!("hit_low:main:{}", hour_fp(NOW - 981.0))]
    );
    assert!(
        got[0].detail.starts_with("hit_low: hit 0.80"),
        "{}",
        got[0].detail
    );
}

fn fleet_state(
    e: &Env,
    fleet: Vec<(String, FleetHour)>,
    starts: Vec<(String, Vec<(f64, String)>)>,
) {
    let st = TokenState {
        fleet: fleet.into_iter().collect(),
        starts: starts.into_iter().collect(),
        ..TokenState::default()
    };
    e.set_state(&st);
}

fn hours(n: usize, units: f64, polls: u64) -> Vec<(String, FleetHour)> {
    (0..n)
        .map(|i| {
            let h = hour_of(NOW - 3600.0 * (n - i) as f64);
            (
                h,
                FleetHour {
                    units,
                    opus: 0.0,
                    polls,
                },
            )
        })
        .collect()
}

#[test]
fn opus_share() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let h = |opus| {
        vec![(
            hour_of(NOW),
            FleetHour {
                units: 100.0,
                opus,
                polls: 0,
            },
        )]
    };
    fleet_state(&e, h(85.0), vec![]);
    assert_eq!(rules(&e.run()), ["opus_share"]);
    fleet_state(&e, h(70.0), vec![]);
    assert!(e.run().is_empty());
}

#[test]
fn poll_rate_and_burn() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let last = hour_of(NOW - 3600.0);
    let mut fleet = hours(30, 100.0, 40);
    fleet
        .iter_mut()
        .filter(|(h, _)| *h == last)
        .for_each(|(_, v)| v.polls = 200);
    fleet_state(&e, fleet, vec![]);
    assert_eq!(
        fps(&e.run()),
        [format!("poll_rate:{}", hour_fp(NOW - 3600.0))]
    );
    let two = [hour_of(NOW - 3600.0), hour_of(NOW - 7200.0)];
    let mut fleet = hours(30, 100.0, 0);
    fleet
        .iter_mut()
        .filter(|(h, _)| two.contains(h))
        .for_each(|(_, v)| v.units = 400.0);
    fleet_state(&e, fleet.clone(), vec![]);
    assert_eq!(fps(&e.run()), [format!("burn:{}", hour_fp(NOW - 3600.0))]);
    fleet
        .iter_mut()
        .filter(|(h, _)| *h == two[1])
        .for_each(|(_, v)| v.units = 100.0);
    fleet_state(&e, fleet, vec![]);
    assert!(e.run().is_empty());
}

#[test]
fn session_start_churn() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let mk = |n: usize| {
        let rows = (0..n)
            .map(|i| (NOW - 60.0 * i as f64, format!("sid{i}")))
            .collect();
        vec![("-Users-x-busy".to_owned(), rows)]
    };
    fleet_state(&e, vec![], mk(6));
    assert!(e.run().is_empty());
    fleet_state(&e, vec![], mk(7));
    let got = e.run();
    assert_eq!(rules(&got), ["session_start_churn"]);
    assert!(got[0].detail.contains("7 main-session starts"));
}

fn user(t: f64, text: &str, entry: &str) -> String {
    let d = json!({"type": "user", "timestamp": iso_ms(t), "entrypoint": entry,
                   "message": {"role": "user", "content": text}});
    format!("{d}\n")
}

#[test]
fn session_start_churn_skips_triage_gate() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let (gate, other) = ("-private-var-T", "-Users-x-busy");
    let gate_text = format!("{TRIAGE_PREFIX} ...");
    for i in 0..8 {
        let t = NOW - 60.0 * f64::from(i);
        let g = [
            user(t, &gate_text, "sdk-cli"),
            line(&format!("mg{i}"), t + 1.0, 0),
        ];
        e.write(&e.main_in(&format!("g{i}"), gate), &g);
        let n = [user(t, "hello", "cli"), line(&format!("mn{i}"), t + 1.0, 0)];
        e.write(&e.main_in(&format!("n{i}"), other), &n);
    }
    let got = e.run();
    assert_eq!(rules(&got), ["session_start_churn"]);
    assert!(got[0].detail.contains("busy") && !got[0].detail.contains("private"));
    let e2 = Env::new(&d.path().join("b"));
    for i in 0..8 {
        let t = NOW - 60.0 * f64::from(i);
        let g = [
            user(t, TRIAGE_PREFIX, "cli"),
            line(&format!("mg{i}"), t + 1.0, 0),
        ];
        e2.write(&e2.main_in(&format!("g{i}"), gate), &g);
    }
    assert_eq!(rules(&e2.run()), ["session_start_churn"]);
}

#[test]
fn fanout() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    for i in 0..3 {
        e.write(
            &e.sub(&format!("fan0000{i}")),
            &[line(&format!("f{i}"), NOW - 30.0, 1000)],
        );
    }
    assert!(e.run().is_empty());
    e.write(&e.sub("fan00009"), &[line("f9", NOW - 20.0, 1000)]);
    assert_eq!(fps(&e.run()), ["fanout:4"]);
}

#[test]
fn hourly_rollup_idempotent() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let ls: Vec<String> = (0..4)
        .map(|i| {
            let l = L {
                read: 100_000,
                out: 50,
                ..D
            };
            line_with(&format!("r{i}"), NOW - 600.0 + f64::from(i), l)
        })
        .collect();
    e.write(&e.main(), &ls);
    let later = NOW + 1900.0;
    e.run_at(later, false);
    let text = fs::read_to_string(d.path().join("hourly.jsonl")).expect("hourly");
    let rows: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("row"))
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0]["turns"].as_u64(), rows[0]["agent_type"].as_str()),
        (Some(4), Some("main"))
    );
    assert_eq!(rows[0]["units_opus"], rows[0]["units"]);
    assert!(rows[0]["units"].as_f64().unwrap_or(0.0) > 0.0);
    fs::remove_file(d.path().join("state.json")).expect("rm");
    e.run_at(later, false);
    let text = fs::read_to_string(d.path().join("hourly.jsonl")).expect("hourly");
    assert_eq!(text.lines().count(), 1);
}

#[test]
fn truncated_last_line_is_picked_up_later() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let p = e.main();
    let whole = line("t2", NOW - 20.0, 320_000);
    e.write(&p, &[line("t1", NOW - 30.0, 1000), whole[..40].to_owned()]);
    assert!(e.run().is_empty());
    e.append(&p, &[whole[40..].to_owned()]);
    assert_eq!(fps(&e.run()), ["ctx_main:315k"]);
    assert_eq!(opens(&e.state()).iter().map(|o| o.turns).sum::<u64>(), 2);
}

#[test]
fn duplicate_message_id_counts_once() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let l = line("same", NOW - 60.0, 10_000);
    e.write(&e.main(), &[l.clone(), l.clone(), l]);
    e.run();
    assert_eq!(opens(&e.state()).iter().map(|o| o.turns).sum::<u64>(), 1);
}

#[test]
fn dry_run_writes_nothing() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    e.write(&e.main(), &[line("d1", NOW - 60.0, 320_000)]);
    assert_eq!(fps(&e.run_at(NOW, true)), ["ctx_main:315k"]);
    assert!(!d.path().join("state.json").exists());
    assert!(!d.path().join("hourly.jsonl").exists());
}

#[test]
fn watcher_degraded_on_bad_lines() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let mut ls = vec!["not json\n".to_owned(); 5];
    ls.extend((0..20).map(|i| line(&format!("g{i}"), NOW - 100.0 + f64::from(i), 1000)));
    e.write(&e.main(), &ls);
    assert_eq!(rules(&e.run()), ["watcher_degraded"]);
}

#[test]
fn no_message_text_in_alerts_or_state() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let secret = "SECRET-PROMPT-TEXT";
    let mut v: Value = serde_json::from_str(&line("z1", NOW - 60.0, 260_000)).expect("json");
    v["message"]["content"] = json!([{"type": "text", "text": secret}]);
    e.write(&e.main(), &[format!("{v}\n")]);
    let got = e.run();
    assert!(!got.is_empty());
    assert!(!format!("{got:?}").contains(secret));
    let st = fs::read_to_string(d.path().join("state.json")).expect("state");
    assert!(!st.contains(secret));
}

#[test]
fn token_alerts_shape_and_failure_contract() {
    let r = TokenAlert {
        rule: "ctx_main".to_owned(),
        session: "tm-x".to_owned(),
        fingerprint: "ctx_main:350k".to_owned(),
        detail: "ctx_main: ctx 360000 >= 350000".to_owned(),
    };
    let a = to_alert(&r, NOW);
    assert_eq!((a.kind.as_str(), a.session.as_str()), ("token", "tm-x"));
    assert_eq!(a.fingerprint, "ctx_main:350k");
    assert_eq!(a.display, "[tm-x]: token ctx_main: ctx 360000 >= 350000");
    let reemit = crate::classify::REEMIT.iter().find(|(k, _)| *k == "token");
    assert_eq!(reemit.map(|(_, s)| *s), Some(3600.0));
    // A state of the wrong shape is a logged failure and no alerts.
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    fs::write(d.path().join("state.json"), r#"{"files": 7}"#).expect("write");
    let paths = Paths {
        base: &e.base,
        sessions: d.path(),
        thresholds: d.path().join("none.json"),
        schedule: d.path(),
    };
    assert!(step(NOW, &e.mode(false), &paths, Some(Default::default())).is_err());
}

fn stopped(mid: &str, t: f64) -> String {
    let mut v: Value = serde_json::from_str(&line(mid, t, 1000)).expect("json");
    v["message"]["stop_reason"] = json!("end_turn");
    format!("{v}\n")
}

#[test]
fn fanout_counts_live_subagents_only() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    for i in 0..2 {
        e.write(
            &e.sub(&format!("live000{i}")),
            &[line(&format!("l{i}"), NOW - 30.0, 1000)],
        );
    }
    for i in 0..2 {
        let ls = [
            line(&format!("d{i}a"), NOW - 60.0, 1000),
            stopped(&format!("d{i}b"), NOW - 20.0),
        ];
        e.write(&e.sub(&format!("done000{i}")), &ls);
    }
    assert!(e.run().is_empty());
    e.append(&e.sub("done0000"), &[line("d0c", NOW - 5.0, 1000)]);
    e.write(&e.sub("live0009"), &[line("l9", NOW - 5.0, 1000)]);
    assert_eq!(fps(&e.run()), ["fanout:4"]);
}

#[test]
fn family_fable_is_not_priced_as_sonnet() {
    let u = json!({"input_tokens": 10, "output_tokens": 1000, "cache_read_input_tokens": 50_000});
    assert_eq!(family("claude-fable-5-2"), "fable");
    assert!(turn_units("claude-fable-5-2", &u) == 0.0);
    assert!(turn_units("claude-sonnet-5-5", &u) > 0.0);
    assert_eq!(family("claude-unheard-9"), "sonnet");
}

fn one_open(think: u64) -> OpenHour {
    OpenHour {
        turns: 1,
        ctx: vec![100],
        out: 5,
        inp: 100,
        think,
        ..OpenHour::default()
    }
}

#[test]
fn rollup_row_flags_fable_cost_unknown() {
    let o = one_open(0);
    assert_eq!(
        rollup_row("h|p|s|main|claude-fable-5-2", &o)["units_unknown"],
        json!(true)
    );
    assert_eq!(
        rollup_row("h|p|s|main|claude-opus-5-5", &o)["units_unknown"],
        json!(false)
    );
}

#[test]
fn effort_and_thinking_in_rollup_and_legacy_key() {
    assert_eq!(effort_of(&json!({"effort": "medium"})), "medium");
    assert_eq!(effort_of(&json!({})), "unknown");
    assert_eq!(
        thinking_of(&json!({"output_tokens_details": {"thinking_tokens": 9}})),
        9
    );
    assert_eq!(thinking_of(&json!({})), 0);
    let r = rollup_row("h|p|s|main|claude-opus-5-5|low", &one_open(4));
    assert_eq!(
        (r["effort"].as_str(), r["thinking_tokens"].as_u64()),
        (Some("low"), Some(4))
    );
    let legacy = rollup_row("h|p|s|main|claude-opus-5-5", &one_open(0));
    assert_eq!(
        (
            legacy["effort"].as_str(),
            legacy["thinking_tokens"].as_u64()
        ),
        (Some("unknown"), Some(0))
    );
}

#[test]
fn poll_command_shapes() {
    assert!(is_poll_command("  tm wait --for run x"));
    assert!(is_poll_command("cd x && sleep 30"));
    assert!(!is_poll_command("tm waiter"));
    assert!(!is_poll_command("nosleep 30"));
    assert!(!is_poll_command("echo tm wait"));
}

#[test]
fn shadow_mode_keeps_its_own_token_state() {
    let d = tempfile::tempdir().expect("tmp");
    let get = |k: &str| match k {
        "ARCHITECT_INBOX_DIR" => Some(d.path().join("scratch").display().to_string()),
        "ARCHITECT_READ_INBOX_DIR" => Some(d.path().join("inbox").display().to_string()),
        "ARCHITECT_DRY_RUN_STATE" => Some(
            d.path()
                .join("scratch/dry-run-state.json")
                .display()
                .to_string(),
        ),
        _ => None,
    };
    let cfg = Config::from_env(d.path(), d.path(), &get).expect("config");
    let m = mode_for(&cfg, true);
    assert_eq!(m.state, d.path().join("scratch").join(STATE_FILE));
    assert_eq!(m.seed, Some(d.path().join("inbox").join(STATE_FILE)));
    assert!(m.save && m.hourly.is_none());
    let mut plain = cfg.clone();
    plain.dry_run_state = None;
    let m = mode_for(&plain, true);
    assert!(
        !m.save && m.hourly.is_none(),
        "a plain dry run writes nothing"
    );
    let m = mode_for(&plain, false);
    assert!(
        m.save && m.hourly.is_some(),
        "a live run writes state and rollup"
    );
}

/// parity window 5, class E: the shadow emitted
/// `trusty-mpm-projects-arch:1f14f88b` `ctx_main:400k` where the poller
/// emitted `tm-architect` `ctx_main:405k`. The name lookup failed for one
/// cycle, so the label fell back and the Architect's bucket swap was skipped.
#[test]
fn window5_unresolved_name_keeps_the_resolved_label() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    let sched = d.path().join("il-schedule.json");
    let bar = json!({"thresholds": {"architect_bar_ctx": 368_182}}).to_string();
    fs::write(&sched, bar).expect("schedule");
    let sid = "1f14f88b-0000";
    let main = e.main_in(sid, "-Users-x-trusty-mpm-projects-architect");
    let run = |names: std::collections::BTreeMap<String, String>| {
        let paths = Paths {
            base: &e.base,
            sessions: d.path(),
            thresholds: d.path().join("none.json"),
            schedule: &sched,
        };
        step(NOW, &e.mode(false), &paths, Some(names)).expect("step")
    };
    e.write(&main, &[line("m1", NOW - 120.0, 190_000)]);
    let named = std::collections::BTreeMap::from([(sid.to_owned(), "tm-architect".to_owned())]);
    assert!(run(named).is_empty());
    // The next cycle's lookup fails; the session crosses the guard.
    e.append(&main, &[line("m2", NOW - 60.0, 405_500)]);
    let got = run(std::collections::BTreeMap::new());
    let seen: Vec<(&str, &str)> = got
        .iter()
        .map(|a| (a.session.as_str(), a.fingerprint.as_str()))
        .collect();
    assert_eq!(seen, [("tm-architect", "ctx_main:405k")]);
}

// #11629: the Rust and Python watchers share one state file; these prove a
// writing scan holds the shared lock and loses nothing to a concurrent writer.

/// One writer's scan of a fresh transcript; returns its path, length and sid.
fn writer_round(
    root: &Path,
    state: &Path,
    w: usize,
    r: usize,
) -> Result<(String, u64, String), String> {
    let base = root.join(format!("base{w}"));
    let sid = format!("w{w}r{r}");
    let p = base
        .join(format!("-srv-proj{w}"))
        .join(format!("{sid}.jsonl"));
    fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    let text = line(&format!("{sid}-m"), NOW - 60.0, 210_000);
    fs::write(&p, &text).expect("write");
    touch(&p);
    let mode = Mode {
        state: state.to_owned(),
        seed: None,
        save: true,
        hourly: None,
    };
    let paths = Paths {
        base: &base,
        sessions: root,
        thresholds: root.join("none.json"),
        schedule: root,
    };
    step(NOW, &mode, &paths, Some(Default::default())).map_err(|e| format!("{sid}: {e}"))?;
    Ok((p.to_string_lossy().into_owned(), text.len() as u64, sid))
}

#[test]
fn concurrent_writers_keep_every_offset_and_fired_key() {
    const WRITERS: usize = 6;
    const ROUNDS: usize = 15;
    let d = tempfile::tempdir().expect("tmp");
    let state = d.path().join("inbox").join(STATE_FILE);
    let results: Vec<Result<(String, u64, String), String>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..WRITERS)
            .map(|w| {
                let (root, state) = (d.path(), state.as_path());
                s.spawn(move || {
                    (0..ROUNDS)
                        .map(|r| writer_round(root, state, w, r))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().expect("writer thread"))
            .collect()
    });
    let errors: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(errors.is_empty(), "scans failed: {errors:?}");
    let st = state::load(&state).expect("state");
    let made: Vec<&(String, u64, String)> =
        results.iter().filter_map(|r| r.as_ref().ok()).collect();
    let lost: Vec<String> = made
        .iter()
        .filter(|(p, len, _)| st.files.get(p).and_then(|f| f.off) != Some(*len))
        .map(|(p, ..)| p.clone())
        .collect();
    assert!(
        lost.is_empty(),
        "lost offsets for {} of {}: {lost:?}",
        lost.len(),
        made.len()
    );
    let unfired: Vec<String> = made
        .iter()
        .filter(|(.., sid)| !st.fired.contains_key(&format!("{sid}|ctx_main:200k")))
        .map(|(.., sid)| sid.clone())
        .collect();
    assert!(unfired.is_empty(), "lost fired keys: {unfired:?}");
    let stray: Vec<String> = fs::read_dir(d.path().join("inbox"))
        .expect("inbox")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
}

#[test]
fn lock_failure_refuses_the_write() {
    let d = tempfile::tempdir().expect("tmp");
    let e = Env::new(d.path());
    e.write(&e.main(), &[line("m0", NOW - 60.0, 210_000)]);
    let mode = e.mode(false);
    // The lock path is a directory, so the lock cannot be opened.
    fs::create_dir_all(lock::lock_path(&mode.state)).expect("mkdir");
    let paths = Paths {
        base: &e.base,
        sessions: d.path(),
        thresholds: d.path().join("none.json"),
        schedule: d.path(),
    };
    let err = step(NOW, &mode, &paths, Some(Default::default())).expect_err("no lock, no write");
    assert!(err.to_string().starts_with("lock "), "{err}");
    assert!(!mode.state.exists(), "the state was written unlocked");
    assert!(!d.path().join("hourly.jsonl").exists());
    // A held lock times out rather than letting a second writer in.
    let state = d.path().join("held").join(STATE_FILE);
    let held = lock::acquire(&state, Duration::from_secs(1)).expect("first holder");
    match lock::acquire(&state, Duration::from_millis(100)) {
        Err(StateError::Io { source, .. }) => {
            assert_eq!(source.kind(), std::io::ErrorKind::TimedOut);
        }
        other => panic!("second writer got {other:?}"),
    }
    drop(held);
    lock::acquire(&state, Duration::from_secs(1)).expect("free after release");
}

/// The Python watcher's lock step in protocol: `<state>.lock`, opened for
/// append, `fcntl.flock(fd, LOCK_EX)`.
const PY_HOLD: &str = "import fcntl, sys\n\
f = open(sys.argv[1] + '.lock', 'a')\n\
fcntl.flock(f.fileno(), fcntl.LOCK_EX)\n\
print('held', flush=True)\n\
sys.stdin.read()\n";

#[test]
fn python_flock_on_the_lock_file_blocks_the_rust_writer() {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let d = tempfile::tempdir().expect("tmp");
    let state = d.path().join(STATE_FILE);
    let name = lock::lock_path(&state);
    assert_eq!(
        name.file_name().and_then(|n| n.to_str()),
        Some("token-watch-state.json.lock")
    );
    let mut py = Command::new("python3")
        .arg("-c")
        .arg(PY_HOLD)
        .arg(&state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3 on PATH");
    let out = py.stdout.take().expect("stdout");
    let mut first = String::new();
    std::io::BufReader::new(out)
        .read_line(&mut first)
        .expect("read");
    assert_eq!(first.trim(), "held");
    let blocked = lock::acquire(&state, Duration::from_millis(200));
    drop(py.stdin.take());
    let status = py.wait().expect("wait");
    assert!(status.success());
    assert!(blocked.is_err(), "Rust took the lock Python held");
    lock::acquire(&state, Duration::from_secs(1)).expect("free once Python exits");
}

/// #11826: the shadow reseeds its token state from the live one, and only
/// under the live watcher's lock; a held lock refuses rather than reads.
#[test]
fn reseed_copies_the_live_state_under_its_lock() {
    let d = tempfile::tempdir().expect("tmp");
    let live = d.path().join("inbox").join(STATE_FILE);
    let own = d.path().join("shadow").join(STATE_FILE);
    let mut st = TokenState::default();
    st.fired
        .insert("tm-a|hit_low:main:2026112420".to_owned(), NOW);
    save(&live, &st).expect("live");
    save(&own, &TokenState::default()).expect("stale own");
    let held = lock::acquire(&live, Duration::from_secs(1)).expect("live holder");
    let err = reseed(&live, &own, Duration::from_millis(100)).expect_err("lock held");
    assert!(err.to_string().starts_with("lock "), "{err}");
    assert!(state::load(&own).expect("own").fired.is_empty());
    drop(held);
    reseed(&live, &own, Duration::from_secs(1)).expect("reseed");
    assert_eq!(state::load(&own).expect("own"), st);
    // No live state: the shadow keeps its own.
    let gone = d.path().join("none").join(STATE_FILE);
    reseed(&gone, &own, Duration::from_secs(1)).expect("absent live");
    assert_eq!(state::load(&own).expect("own"), st);
}
