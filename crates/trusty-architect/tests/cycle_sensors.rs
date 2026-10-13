//! #9171 A3b-2: the cycle's sensor wiring: the transcript fallback through
//! a whole cycle (A3a critic carry-over), the wake pointer's "need you" form
//! from a cycle, and a bad `LOAD_FACTOR` (Architect ruling Q4).

mod cycle_support;

use std::fs;
use std::io;
use std::path::Path;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use trusty_architect::FakeClock;
use trusty_architect::poller::{DiskUsage, HostProbe, PollSession, PollState, host_alerts};

/// A live registry entry mapping `pane` to `sid`, and that session's
/// transcript at `tokens` (input + cache read).
fn transcript(root: &Path, pane: &str, sid: &str, tokens: u64) -> io::Result<()> {
    let sdir = root.join("sessions");
    fs::create_dir_all(&sdir)?;
    let entry = serde_json::json!({"pid": std::process::id(), "sessionId": sid,
        "tmux": format!("tm-a:@1.{pane}"), "updatedAt": 1});
    fs::write(sdir.join("1.json"), entry.to_string())?;
    let d = root.join("projects/-w");
    fs::create_dir_all(&d)?;
    let usage = serde_json::json!({"input_tokens": 10, "cache_read_input_tokens": tokens - 10});
    let line =
        serde_json::json!({"type": "assistant", "message": {"usage": usage}, "entrypoint": "cli"});
    fs::write(d.join(format!("{sid}.jsonl")), line.to_string() + "\n")
}

/// `(kind, fingerprint, detail)` of the `ctx` alerts written for `tm-a`.
fn ctx_rows(env: &Env) -> TestResult<Vec<(String, String, String)>> {
    let text = fs::read_to_string(env.config.alerts()).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        if v["session"] == "tm-a" && v["kind"] == "ctx" {
            out.push((
                "ctx".to_owned(),
                v["fingerprint"].as_str().unwrap_or_default().to_owned(),
                v["detail"].as_str().unwrap_or_default().to_owned(),
            ));
        }
    }
    Ok(out)
}

/// A status line with no `ctx N%` takes the pane's own transcript, inside a
/// whole cycle. Red against a cycle that skips the fallback.
#[test]
fn cycle_falls_back_to_transcript_when_status_ctx_is_none() -> TestResult {
    let env = Env::new(&[])?;
    transcript(env.root(), "%2", "sid-a", 300_000)?;
    let cap = screen(10).replace("ctx 10% |", "⤵ 969k→77k (92%) |");
    let tmux = fleet(&[("tm-a", "%2", "claude")], &cap);
    let clock = FakeClock::new(0.0);
    let mut session = PollSession::new(PollState::default());
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert_eq!(
        ctx_rows(&env)?,
        [("ctx".to_owned(), "25".to_owned(), "ctx 30%".to_owned())]
    );
    Ok(())
}

/// A status line with `ctx N%` wins; the transcript's higher figure is not
/// used.
#[test]
fn cycle_prefers_status_line_ctx() -> TestResult {
    let env = Env::new(&[])?;
    transcript(env.root(), "%2", "sid-a", 300_000)?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(10));
    let clock = FakeClock::new(0.0);
    let mut session = PollSession::new(PollState::default());
    env.runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    assert!(ctx_rows(&env)?.is_empty());
    Ok(())
}

/// The live `cycle` passes triage's logged count (0 with the default triage),
/// so the pointer reads `N alerts need you (0 logged) -> <alerts.jsonl>`.
#[test]
fn wake_message_uses_need_you_form_from_cycle() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fleet(
        &[("tm-architect", "%1", "claude"), ("tm-a", "%2", "claude")],
        &screen(50),
    )
    .fallback("display-message", "tm-architect\n")
    .fallback("send-keys", "");
    let clock = FakeClock::new(0.0);
    let mut session = PollSession::new(PollState::default());
    let summary = env
        .runner(&tmux, &clock, &QuietHost)
        .run_once(&mut session)?;
    // tm-a's ctx alert and the first band alert.
    assert_eq!(
        (summary.new.len(), summary.woken, summary.logged),
        (2, 2, 0)
    );
    let typed = &tmux.sends()[0];
    let want = format!(
        "[poll 00:00Z] 2 alerts need you (0 logged) -> {}",
        env.config.alerts().display()
    );
    assert_eq!(typed.last(), Some(&want), "{typed:?}");
    assert!(
        tmux.sends().iter().all(|c| c[2] == "%1"),
        "keys only to the Architect"
    );
    Ok(())
}

/// A host far over every line.
struct Overloaded;

impl HostProbe for Overloaded {
    fn loadavg(&self) -> io::Result<[f64; 3]> {
        Ok([99.0; 3])
    }
    fn cores(&self) -> usize {
        1
    }
    fn data_volume(&self) -> std::path::PathBuf {
        "/data".into()
    }
    fn disk_usage(&self, _: &Path) -> io::Result<DiskUsage> {
        Ok(DiskUsage {
            total: 100,
            used: 99,
            free: 1,
        })
    }
    fn swap_used_mb(&self) -> io::Result<Option<f64>> {
        Ok(Some(500.0))
    }
    fn mem_free_pct(&self) -> Option<f64> {
        Some(1.0)
    }
}

/// Architect ruling Q4: a bad `LOAD_FACTOR` builds the config, is reported
/// in `Config::problems`, and turns off host alerts only; the cycle still
/// writes pane alerts. A recorded divergence: the prototype aborts the whole
/// cycle. Red against a config that refuses to build.
#[test]
fn bad_load_factor_disables_host_alerts_only() -> TestResult {
    let env = Env::new(&[("LOAD_FACTOR", "lots")])?;
    assert_eq!(env.config.load_factor, None);
    let names: Vec<_> = env.config.problems.iter().map(|p| p.name).collect();
    assert_eq!(names, ["LOAD_FACTOR"]);

    let mut band = None;
    let host = host_alerts(&Overloaded, &env.config, 0.0, Some(&mut band))?;
    assert!(host.is_empty(), "{host:?}");
    assert_eq!(band, None, "the band is a host alert too");

    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(0.0);
    let mut session = PollSession::new(PollState::default());
    env.runner(&tmux, &clock, &Overloaded)
        .run_once(&mut session)?;
    assert_eq!(env.written("tm-a")?, ["ctx"]);
    assert!(env.written("")?.is_empty(), "no load, disk, swap or band");
    Ok(())
}

/// #9171 A3b-2 critic HIGH: a bad variable is reported by every cycle, not
/// once at loop start, and the line names the sensor that is off. Called on
/// `run_once` alone, so it holds without `run_loop`. Red against a poller
/// that logs `Config::problems` only when the loop starts.
#[test]
fn every_cycle_names_each_sensor_a_bad_variable_turned_off() -> TestResult {
    let env = Env::new(&[("LOAD_FACTOR", "lots"), ("SELF_CTX_THRESHOLD", "x")])?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(50));
    let clock = FakeClock::new(0.0);
    let runner = env.runner(&tmux, &clock, &Overloaded);
    let mut session = PollSession::new(PollState::default());
    let errors = |log: &str, needle: &str| {
        log.lines()
            .filter(|l| l.contains("config ERROR") && l.contains(needle))
            .count()
    };
    for cycle in 1..=2 {
        runner.run_once(&mut session)?;
        let log = env.log();
        assert_eq!(errors(&log, "LOAD_FACTOR"), cycle, "cycle {cycle}: {log}");
        assert_eq!(
            errors(&log, "SELF_CTX_THRESHOLD"),
            cycle,
            "cycle {cycle}: {log}"
        );
        assert_eq!(
            errors(&log, "host alerts"),
            cycle,
            "names the sensor: {log}"
        );
        assert_eq!(errors(&log, "self_ctx"), cycle, "names the sensor: {log}");
    }
    Ok(())
}

/// A good `LOAD_FACTOR` keeps the host alerts on.
#[test]
fn good_load_factor_keeps_host_alerts() -> TestResult {
    let env = Env::new(&[("LOAD_FACTOR", "2")])?;
    assert_eq!(env.config.load_factor, Some(2.0));
    assert!(env.config.problems.is_empty());
    let host = host_alerts(&Overloaded, &env.config, 0.0, None)?;
    let kinds: Vec<_> = host.iter().map(|a| a.kind.as_str()).collect();
    assert_eq!(kinds, ["load", "disk", "swap"]);
    Ok(())
}
