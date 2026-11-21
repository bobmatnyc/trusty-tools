//! df-T7i P1b: the prototype tests added since 988152c, ported by name, plus
//! one error-arm test for every new failure branch (Fail-Open Check).
//!
//! What: `scripts/test_fleet_poll_{band,ctx,d22,idle,zone}.py` by name. The
//! one ruled divergence is `test_wake_confirms_by_marker_when_box_shows_only_part`:
//! the port submits only when the box equals the typed line (A3 Q3), so a
//! box showing only the head is not confirmed.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};
use trusty_architect::classify::{
    BoxReading, CTX_HIGH, CTX_WARN, PaneInfo, PaneState, evaluate, parse_pane,
};
use trusty_architect::poller::digest::{WAKE_ALERTS_MAX, WakeItem, bound_wake_alerts, cap_wake};
use trusty_architect::poller::self_refresh::self_refresh_step;
use trusty_architect::poller::steps::scripts::{ChildOutput, ScriptSteps, Subprocess};
use trusty_architect::poller::zone::{parse_zprint, zone_alerts};
use trusty_architect::poller::{
    BAND_SWAP_ACTIVE_PAGES, BandConfig, Config, Pane, PollState, SELF_CTX_THRESHOLD_DEFAULT,
    StoredCount, WakeContext, builder_band, completion_alerts, dispatch_wake_with,
    parse_vm_stat_swaps, record_wake_result,
};
use trusty_architect::{FakeClock, InputState, PaneId, ScriptedTmux};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RULE: &str = "────────────────────────────────────────";

fn idle_capture() -> String {
    format!("⏺ Done.\n\n{RULE}\n❯\u{a0}\n{RULE}\n  ⏵⏵ bypass\n")
}

/// The prototype's `_info()`: an empty, idle box at ctx 20%.
fn info() -> PaneInfo {
    let mut i = parse_pane(&idle_capture());
    i.ctx = Some(20);
    i
}

// --- scripts/test_fleet_poll_band.py ---------------------------------------------

fn band(pages: f64) -> &'static str {
    builder_band(
        &BandConfig::default(),
        2.0,
        16,
        500.0,
        Some(60.0),
        Some(pages),
        Some("open"),
    )
    .as_str()
}

// Python: test_tiny_swap_deltas_do_not_stop_band
#[test]
fn test_tiny_swap_deltas_do_not_stop_band() {
    for pages in [0.0, 4.0, 8.0, 28.0, BAND_SWAP_ACTIVE_PAGES - 1.0] {
        assert_eq!(band(pages), "open", "{pages}");
    }
}

// Python: test_large_swap_delta_stops_band
#[test]
fn test_large_swap_delta_stops_band() {
    assert_eq!(band(BAND_SWAP_ACTIVE_PAGES), "stop");
    assert_eq!(band(5000.0), "stop");
}

// --- scripts/test_fleet_poll_ctx.py ----------------------------------------------

// Python: test_ctx_alert_at_the_new_bar
#[test]
fn test_ctx_alert_at_the_new_bar() {
    assert_eq!(
        (CTX_WARN, CTX_HIGH, SELF_CTX_THRESHOLD_DEFAULT),
        (31, 35, 35)
    );
    let fps = |ctx| {
        let mut i = info();
        i.ctx = Some(ctx);
        evaluate(&i, &mut PaneState::default(), 1000.0, "tm-x", "%1")
            .into_iter()
            .filter(|a| a.kind == "ctx")
            .map(|a| a.fingerprint)
            .collect::<Vec<_>>()
    };
    assert_eq!(fps(34), ["31"]);
    assert_eq!(fps(35), ["35"]);
    assert!(fps(30).is_empty());
}

fn complete(other: &mut Map<String, Value>, i: &PaneInfo, now: f64, il: bool) -> Vec<String> {
    completion_alerts(other, "tm-x", "%1", i, now, &|_| il)
        .into_iter()
        .map(|a| format!("{}:{}:{}", a.kind, a.fingerprint, a.session))
        .collect()
}

// Python: test_completion_fires_once_per_completion
#[test]
fn test_completion_fires_once_per_completion() {
    let mut st = Map::new();
    assert_eq!(
        complete(&mut st, &info(), 1000.0, true),
        ["ctx:complete:tm-x"]
    );
    assert!(
        complete(&mut st, &info(), 1060.0, true).is_empty(),
        "same completion"
    );
    let mut busy = info();
    busy.busy = true;
    assert!(
        complete(&mut st, &busy, 1120.0, true).is_empty(),
        "new work"
    );
    assert_eq!(
        complete(&mut st, &info(), 1180.0, true).len(),
        1,
        "next completion"
    );
}

// Python: test_completion_never_with_draft_agent_or_open_il
#[test]
fn test_completion_never_with_draft_agent_or_open_il() {
    let mut typed = info();
    typed.box_reading = BoxReading::Input(InputState::Typed);
    let mut agent = info();
    (agent.agents, agent.busy) = (1, true);
    let mut dialog = info();
    dialog.dialog = Some(("d".to_owned(), "d".to_owned()));
    let mut low = info();
    low.ctx = Some(14);
    let mut none = info();
    none.ctx = None;
    for (i, il) in [
        (typed, true),
        (agent, true),
        (dialog, true),
        (info(), false),
        (low, true),
        (none, true),
    ] {
        assert!(
            complete(&mut Map::new(), &i, 1000.0, il).is_empty(),
            "{i:?} {il}"
        );
    }
}

// --- scripts/test_fleet_poll_d22.py ------------------------------------------------

// Python: test_cap_wake_limits_length_keeps_head_and_marks_cut
#[test]
fn test_cap_wake_limits_length_keeps_head_and_marks_cut() {
    let head = "[poll 07:00Z] 3 alerts need you (1 logged) -> /x/inbox/alerts.jsonl";
    let out = cap_wake(head, &"item ".repeat(300));
    assert!(out.chars().count() <= 400 && out.starts_with(head) && out.ends_with('…'));
    assert!(!out.contains('\n'));
}

// Python: test_cap_wake_drops_digest_when_no_room
#[test]
fn test_cap_wake_drops_digest_when_no_room() {
    let head = "h".repeat(390);
    assert_eq!(cap_wake(&head, "digest text here"), head);
}

/// Ruled (A3 Q3): Enter only when the box equals the whole typed line. A box
/// showing only the head is not confirmed and nothing is submitted.
// Python: test_wake_confirms_by_marker_when_box_shows_only_part
#[test]
fn test_wake_confirms_by_marker_when_box_shows_only_part() -> TestResult {
    let empty = format!("{RULE}\n❯\u{a0}\n{RULE}\n");
    let tmux = ScriptedTmux::new()
        .fallback("capture-pane", &empty)
        .fallback("display-message", "tm-architect\n")
        .fallback("send-keys", "");
    let clock = FakeClock::new(0.0);
    let mut state = PollState::default();
    let woken = [WakeItem {
        session: Some("tm-a".to_owned()),
        kind: Some("idle".to_owned()),
        detail: Some("d ".repeat(500)),
    }];
    let ctx = WakeContext {
        woken: &woken,
        ..WakeContext::default()
    };
    let result = dispatch_wake_with(&mut state, 2, None, false, &clock, &ctx, |_, _, digest| {
        assert!(!digest.is_empty());
        Ok("failed: text not confirmed in the input box; box changed, nothing erased".to_owned())
    });
    assert!(result.starts_with("failed"), "{result}");
    assert!(
        !tmux
            .sends()
            .iter()
            .any(|c| c.last().map(String::as_str) == Some("Enter"))
    );
    Ok(())
}

// Python: test_wake_alerts_deduped_capped_and_counted
#[test]
fn test_wake_alerts_deduped_capped_and_counted() {
    let pend: Vec<WakeItem> = (0..2000)
        .map(|i| WakeItem {
            session: Some("s".to_owned()),
            kind: Some("k".to_owned()),
            detail: Some(format!("d{}", i % 80)),
        })
        .collect();
    let mut other = Map::new();
    let kept = bound_wake_alerts(&mut other, pend);
    assert_eq!(kept.len(), WAKE_ALERTS_MAX);
    assert_eq!(kept.last().and_then(|a| a.detail.as_deref()), Some("d79"));
    assert_eq!(other["wake_alerts_dropped"], 2000 - WAKE_ALERTS_MAX as u64);
}

// Python: test_idle_swap_does_not_hold_band_at_stop
#[test]
fn test_idle_swap_does_not_hold_band_at_stop() {
    let b = |pages: f64| {
        builder_band(
            &BandConfig::default(),
            2.0,
            16,
            500.0,
            Some(60.0),
            Some(pages),
            Some("stop"),
        )
    };
    assert_eq!(b(0.0).as_str(), "open");
    assert_eq!(b(300.0).as_str(), "stop");
}

/// The delta of two `vm_stat` reads.
// Python: test_swap_activity_is_counter_delta
#[test]
fn test_swap_activity_is_counter_delta() -> io::Result<()> {
    let first = parse_vm_stat_swaps("Swapins: 10.\nSwapouts: 5.\n")?;
    let second = parse_vm_stat_swaps("Swapins: 14.\nSwapouts: 9.\n")?;
    assert_eq!(second.saturating_sub(first), 8);
    Ok(())
}

/// A recording [`Subprocess`]: scripted output per program, every call kept.
#[derive(Default)]
struct FakeRun {
    out: Vec<(&'static str, io::Result<ChildOutput>)>,
    spawn_fails: bool,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Subprocess for FakeRun {
    fn spawn_detached(&self, argv: &[String], _: Option<&Path>) -> io::Result<u32> {
        self.calls.borrow_mut().push(argv.to_vec());
        if self.spawn_fails {
            return Err(io::Error::other("spawn refused"));
        }
        Ok(4242)
    }
    fn output(&self, argv: &[String], _: Duration) -> io::Result<ChildOutput> {
        self.calls.borrow_mut().push(argv.to_vec());
        for (prog, out) in &self.out {
            if argv.first().is_some_and(|a| a == prog) {
                return match out {
                    Ok(o) => Ok(o.clone()),
                    Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
                };
            }
        }
        Ok(ChildOutput {
            code: Some(1),
            ..ChildOutput::default()
        })
    }
}

fn config(root: &Path, vars: &[(&str, &str)]) -> Result<Config, Box<dyn std::error::Error>> {
    let owned: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let get = move |k: &str| owned.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    Ok(Config::from_env(root, root, &get)?)
}

fn transcript(dir: &Path, tokens: i64) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let line = json!({"type": "assistant", "message": {"usage": {"input_tokens": tokens}}, "entrypoint": "cli"});
    fs::write(dir.join("t.jsonl"), format!("{line}\n"))
}

fn sup_panes() -> Result<Vec<Pane>, Box<dyn std::error::Error>> {
    Ok(vec![Pane {
        session: "tm-architect".to_owned(),
        pane: PaneId::parse("%1")?,
        cmd: "claude".to_owned(),
        active: "11".to_owned(),
        path: String::new(),
        dead: String::new(),
    }])
}

/// The prototype's `_refresh`: (phrase, starts) at `pct`% of a 1M window.
fn refresh(
    other: &mut Map<String, Value>,
    pct: i64,
    now: f64,
    running: bool,
    capture: &str,
) -> Result<(String, usize), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let ctx_dir = root.path().join("ctx");
    transcript(&ctx_dir, pct * 10_000)?;
    let cfg = config(root.path(), &[("SELF_CTX_DIR", &ctx_dir.to_string_lossy())])?;
    let tmux = ScriptedTmux::new().fallback("capture-pane", capture);
    let pgrep = ChildOutput {
        code: Some(if running { 0 } else { 1 }),
        ..ChildOutput::default()
    };
    let run = FakeRun {
        out: vec![("pgrep", Ok(pgrep))],
        ..FakeRun::default()
    };
    let mut log = Vec::new();
    let phrase = self_refresh_step(
        other,
        &cfg,
        &tmux,
        &run,
        &sup_panes()?,
        now,
        false,
        &mut log,
    );
    let starts = run
        .calls
        .borrow()
        .iter()
        .filter(|c| c.first().map(String::as_str) == Some("bash"))
        .count();
    Ok((phrase, starts))
}

// Python: test_self_refresh_starts_once_at_50_percent_when_idle
#[test]
fn test_self_refresh_starts_once_at_50_percent_when_idle() -> TestResult {
    let mut st = Map::new();
    let (out, started) = refresh(&mut st, 50, 10_000.0, false, &idle_capture())?;
    assert!(
        out.starts_with("self-refresh=started") && started == 1,
        "{out}"
    );
    assert_eq!(st["self_refresh"]["pid"], 4242);
    let (out2, started2) = refresh(&mut st, 55, 10_060.0, false, &idle_capture())?;
    assert_eq!((out2.as_str(), started2), ("self-refresh=debounced", 0));
    Ok(())
}

// Python: test_self_refresh_skips_below_threshold_running_and_busy
#[test]
fn test_self_refresh_skips_below_threshold_running_and_busy() -> TestResult {
    let idle = idle_capture();
    assert_eq!(
        refresh(&mut Map::new(), 49, 10_000.0, false, &idle)?,
        (String::new(), 0)
    );
    assert_eq!(
        refresh(&mut Map::new(), 60, 10_000.0, true, &idle)?,
        ("self-refresh=running".to_owned(), 0)
    );
    let busy = format!("✻ Thinking… (6s · esc to interrupt)\n{RULE}\n❯\u{a0}\n{RULE}\n");
    assert_eq!(
        refresh(&mut Map::new(), 60, 10_000.0, false, &busy)?,
        ("self-refresh=not idle".to_owned(), 0)
    );
    let typed = format!("⏺ Done.\n\n{RULE}\n❯\u{a0}half a draft\n{RULE}\n");
    assert_eq!(
        refresh(&mut Map::new(), 60, 10_000.0, false, &typed)?,
        ("self-refresh=not idle".to_owned(), 0)
    );
    Ok(())
}

/// Error arm: a failed `self-clear.sh` start is an error phrase and a log
/// line; nothing is stored (the prototype raises and loses the cycle).
#[test]
fn a_failed_self_clear_start_is_an_error_phrase() -> TestResult {
    let root = tempfile::tempdir()?;
    let ctx_dir = root.path().join("ctx");
    transcript(&ctx_dir, 600_000)?;
    let cfg = config(root.path(), &[("SELF_CTX_DIR", &ctx_dir.to_string_lossy())])?;
    let tmux = ScriptedTmux::new().fallback("capture-pane", &idle_capture());
    let run = FakeRun {
        spawn_fails: true,
        ..FakeRun::default()
    };
    let mut other = Map::new();
    let mut log = Vec::new();
    let phrase = self_refresh_step(
        &mut other,
        &cfg,
        &tmux,
        &run,
        &sup_panes()?,
        10_000.0,
        false,
        &mut log,
    );
    assert_eq!(phrase, "self-refresh=error");
    assert!(
        log.iter().any(|l| l.contains("self_refresh start ERROR")),
        "{log:?}"
    );
    assert_eq!(other["self_refresh"], json!({}));
    // A bad SELF_REFRESH_PCT is an error phrase too, as in the prototype.
    let bad = config(root.path(), &[("SELF_REFRESH_PCT", "lots")])?;
    let phrase = self_refresh_step(
        &mut other,
        &bad,
        &tmux,
        &run,
        &sup_panes()?,
        0.0,
        false,
        &mut log,
    );
    assert_eq!(phrase, "self-refresh=error");
    Ok(())
}

// --- scripts/test_fleet_poll_idle.py ------------------------------------------------

/// The cycle's quiet filter over `evaluate`.
// Python: test_muted_session_drops_idle_only
#[test]
fn test_muted_session_drops_idle_only() {
    let mut i = info();
    i.ctx = Some(90);
    let kinds = |session: &str| {
        let mut ps = PaneState {
            idle_since: Some(0.0),
            ..PaneState::default()
        };
        let quiet = ["tm-personal".to_owned()];
        evaluate(&i, &mut ps, 10_000.0, session, "p")
            .into_iter()
            .filter(|a| !(a.kind == "idle" && quiet.contains(&a.session)))
            .map(|a| a.kind)
            .collect::<Vec<_>>()
    };
    assert!(!kinds("tm-personal").contains(&"idle".to_owned()));
    assert!(kinds("tm-personal").contains(&"ctx".to_owned()));
    assert!(kinds("tm-other").contains(&"idle".to_owned()));
}

// Python: test_exempt_session_gets_no_idle_alert_but_keeps_ctx
#[test]
fn test_exempt_session_gets_no_idle_alert_but_keeps_ctx() {
    let mut i = info();
    i.ctx = Some(90);
    let kinds = |session: &str| {
        let mut ps = PaneState {
            idle_since: Some(0.0),
            ..PaneState::default()
        };
        evaluate(&i, &mut ps, 10_000.0, session, "p")
            .into_iter()
            .map(|a| a.kind)
            .collect::<Vec<_>>()
    };
    let q = kinds("tm-architect-questions");
    assert!(!q.contains(&"idle".to_owned()) && q.contains(&"ctx".to_owned()));
    assert!(kinds("tm-other-01").contains(&"idle".to_owned()));
}

// --- scripts/test_fleet_poll_zone.py --------------------------------------------------

const BOOT: i64 = 1_000_000;

// Python: test_reads_field_seven_of_zprint
#[test]
fn test_reads_field_seven_of_zprint() {
    let zprint = "zone name elem_size cur_size max_size cur_elts max_elts inuse\n\
                  data.kalloc.512 512 1 2 99 100 90\n\
                  data.kalloc.1024 1024 1 2 9000000 9000000 8500000\n";
    assert_eq!(parse_zprint(zprint), Some(8_500_000));
}

fn zone_step(
    st: &mut Map<String, Value>,
    now: i64,
    elements: u64,
    log: &Path,
    boot: i64,
) -> Vec<String> {
    zone_alerts(st, now as f64, || (Some(elements), Some(boot)), log)
        .0
        .into_iter()
        .map(|a| a.detail)
        .collect()
}

// Python: test_hourly_log_and_threshold_dedup_per_boot
#[test]
fn test_hourly_log_and_threshold_dedup_per_boot() -> TestResult {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("z.jsonl");
    let mut st = Map::new();
    assert!(zone_step(&mut st, BOOT + 100, 1_000_000, &log, BOOT).is_empty());
    assert!(
        zone_step(&mut st, BOOT + 200, 9_000_000, &log, BOOT).is_empty(),
        "inside the hour"
    );
    let a = zone_step(&mut st, BOOT + 3700, 9_000_000, &log, BOOT);
    assert!(a.len() == 1 && a[0].contains("alert Bob"));
    assert!(
        zone_step(&mut st, BOOT + 7400, 9_500_000, &log, BOOT).is_empty(),
        "same boot"
    );
    let b = zone_step(&mut st, BOOT + 11_100, 12_500_000, &log, BOOT);
    assert!(b.len() == 1 && b[0].contains("reboot now"));
    let rows: Vec<Value> = fs::read_to_string(&log)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert!(rows.len() == 4 && rows[0]["elements"] == 1_000_000 && rows[0]["uptime_s"] == 100);
    let c = zone_step(&mut st, BOOT + 20_000, 9_000_000, &log, BOOT + 15_000);
    assert!(
        c.len() == 1 && c[0].contains("alert Bob"),
        "a new boot re-arms"
    );
    Ok(())
}

// Python: test_uptime_over_14_days_alerts_once
#[test]
fn test_uptime_over_14_days_alerts_once() -> TestResult {
    let dir = tempfile::tempdir()?;
    let log = dir.path().join("z.jsonl");
    let mut st = Map::new();
    let a = zone_step(&mut st, BOOT + 15 * 86_400, 1_000_000, &log, BOOT);
    assert!(a.len() == 1 && a[0].contains("planned reboot due"));
    assert!(zone_step(&mut st, BOOT + 15 * 86_400 + 3700, 1_000_000, &log, BOOT).is_empty());
    Ok(())
}

/// Error arm: a zone log that cannot be written is a log line; the alert
/// still fires.
#[test]
fn a_zone_log_write_failure_is_a_log_line() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut st = Map::new();
    let (alerts, log) = zone_alerts(&mut st, 4000.0, || (Some(9_000_000), Some(0)), dir.path());
    assert_eq!(alerts.len(), 1);
    assert!(
        log.iter().any(|l| l.starts_with("zone log ERROR")),
        "{log:?}"
    );
    let (none, log) = zone_alerts(&mut Map::new(), 4000.0, || (None, None), dir.path());
    assert!(none.is_empty() && log == ["zone reading unavailable"]);
    Ok(())
}

// --- wake: the self-clear marker and the D39 record -------------------------------------

// Python: test_dispatch_wake_suppressed_while_self_clear_marker_fresh
#[test]
fn test_dispatch_wake_suppressed_while_self_clear_marker_fresh() -> TestResult {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("wake-suppress");
    fs::write(&marker, "")?;
    let now = fs::metadata(&marker)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs_f64();
    let ctx = WakeContext {
        suppress: Some(&marker),
        ..WakeContext::default()
    };
    let mut state = PollState::default();
    let clock = FakeClock::new(now + 60.0);
    let r = dispatch_wake_with(&mut state, 2, None, false, &clock, &ctx, |_, _, _| {
        Ok("sent to %1".to_owned())
    });
    assert_eq!(r, "suppressed: self-clear waiting for idle");
    assert_eq!(
        state.wake_pending,
        Some(StoredCount::Count(2)),
        "still owed"
    );
    // A marker older than three hours is stale (self-clear killed): wake as usual.
    let stale = FakeClock::new(now + 3.0 * 3600.0 + 1.0);
    let r = dispatch_wake_with(&mut state, 0, None, false, &stale, &ctx, |_, _, _| {
        Ok("sent to %1".to_owned())
    });
    assert_eq!(r, "sent to %1");
    Ok(())
}

/// Error arm: a wake-alert record that cannot be written never changes the
/// wake's result or its state.
#[test]
fn a_failed_wake_alert_record_never_changes_the_wake() -> TestResult {
    let dir = tempfile::tempdir()?;
    let blocked = dir.path().join("not-a-dir").join("pm-wake-alert.json");
    fs::write(dir.path().join("not-a-dir"), "file")?;
    let ctx = WakeContext {
        wake_alert: Some(&blocked),
        ..WakeContext::default()
    };
    let clock = FakeClock::new(1000.0);
    let mut state = PollState::default();
    let r = dispatch_wake_with(&mut state, 1, None, false, &clock, &ctx, |_, _, _| {
        Ok("failed: no pane".to_owned())
    });
    assert_eq!(r, "failed: no pane");
    assert_eq!(state.wake_pending, Some(StoredCount::Count(1)));
    assert!(record_wake_result(&blocked, "failed: x", 0.0).is_err());
    Ok(())
}

/// D39: failures count attempts and keep the first time; a sent wake clears.
#[test]
fn wake_alert_record_counts_failures_and_clears_on_sent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("pm-wake-alert.json");
    record_wake_result(&path, "skipped: dialog open", 0.0)?;
    assert!(!path.exists(), "a skip changes nothing");
    record_wake_result(&path, "failed: a", 0.0)?;
    record_wake_result(&path, "failed: b", 60.0)?;
    let st: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    assert_eq!(
        st,
        json!({"first_ts": "1970-01-01T00:00:00Z", "attempts": 2, "last_ts": "1970-01-01T00:01:00Z"})
    );
    record_wake_result(&path, "sent to %1", 120.0)?;
    assert!(!path.exists());
    record_wake_result(&path, "sent to %1", 120.0)?;
    Ok(())
}

// --- the run_* script steps --------------------------------------------------------------

fn steps_config(
    off: &[(&str, &str)],
) -> Result<(tempfile::TempDir, Config), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let cfg = config(root.path(), off)?;
    Ok((root, cfg))
}

fn names(run: &FakeRun) -> Vec<String> {
    run.calls
        .borrow()
        .iter()
        .map(|c| {
            let script = c
                .iter()
                .find(|a| a.ends_with(".py"))
                .cloned()
                .unwrap_or_default();
            let base = Path::new(&script)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let extra: Vec<&str> = c
                .iter()
                .skip_while(|a| !a.ends_with(".py"))
                .skip(1)
                .map(String::as_str)
                .collect();
            [base]
                .into_iter()
                .chain(extra.iter().map(|s| (*s).to_owned()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn script_steps_follow_the_prototype_cadence() -> TestResult {
    let (_root, cfg) = steps_config(&[])?;
    let run = FakeRun::default();
    let steps = ScriptSteps::new(&run, &cfg);
    steps.run(1000.0);
    assert_eq!(
        names(&run),
        [
            "board_feed.py",
            "share_sync.py poll",
            "questions_rc.py",
            "task_ingest.py --sweep",
            "task_progress.py",
            "il_dashboard_sync.py --pm-sweep",
            "track_gh.py",
            "runtime_selftest.py"
        ]
    );
    run.calls.borrow_mut().clear();
    steps.run(1059.0);
    assert_eq!(
        names(&run),
        ["board_feed.py", "share_sync.py poll", "questions_rc.py"]
    );
    run.calls.borrow_mut().clear();
    steps.run(1060.0);
    assert_eq!(
        &names(&run)[3..],
        [
            "task_ingest.py --sweep",
            "task_progress.py",
            "il_dashboard_sync.py"
        ]
    );
    run.calls.borrow_mut().clear();
    steps.run(1300.0);
    assert_eq!(
        &names(&run)[3..],
        [
            "task_ingest.py --sweep",
            "task_progress.py",
            "il_dashboard_sync.py --pm-sweep",
            "track_gh.py",
            "runtime_selftest.py"
        ]
    );
    Ok(())
}

#[test]
fn script_steps_honour_off_switches() -> TestResult {
    let vars = [
        ("SHARE_SYNC_POLL", "off"),
        ("TASK_SWEEP", "off"),
        ("TASK_PROGRESS", "off"),
        ("IL_DASH_SYNC", "off"),
        ("TRACK_GH", "off"),
        ("RUNTIME_SELFTEST", "off"),
    ];
    let (_root, cfg) = steps_config(&vars)?;
    let run = FakeRun::default();
    ScriptSteps::new(&run, &cfg).run(1000.0);
    assert_eq!(names(&run), ["board_feed.py", "questions_rc.py"]);
    Ok(())
}

/// Error arm: a script that times out or cannot start is one log line; the
/// other steps still run.
#[test]
fn a_failing_script_is_a_log_line() -> TestResult {
    let (_root, cfg) = steps_config(&[])?;
    let timeout = io::Error::new(io::ErrorKind::TimedOut, "timed out after 240 seconds");
    let ok = ChildOutput {
        code: Some(0),
        stdout: "l1\nl2\n".to_owned(),
        stderr: "e1\n".to_owned(),
    };
    let run = FakeRun {
        out: vec![("python3", Err(timeout))],
        spawn_fails: true,
        ..FakeRun::default()
    };
    let log = ScriptSteps::new(&run, &cfg).run(1000.0);
    assert!(
        log.contains(&"board feed ERROR timed out after 240 seconds".to_owned()),
        "{log:?}"
    );
    assert!(
        log.contains(&"track_gh ERROR spawn refused".to_owned()),
        "{log:?}"
    );
    assert_eq!(names(&run).len(), 8, "every step still ran");
    let run = FakeRun {
        out: vec![("python3", Ok(ok))],
        ..FakeRun::default()
    };
    let log = ScriptSteps::new(&run, &cfg).run(1000.0);
    assert_eq!(log, ["l1", "l2", "l1", "l2", "e1", "l1", "l2", "e1"]);
    Ok(())
}
