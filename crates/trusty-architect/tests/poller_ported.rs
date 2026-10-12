//! #9171 A3a: the prototype's sensor and state tests, ported by name against
//! `trusty_architect::poller`.
//!
//! Sources: `tests/test_fleet_poll.py` (events, self_ctx, transcript ctx,
//! load, band), `scripts/test_fleet_poll_band.py` and
//! `scripts/test_fleet_poll_ctx.py` at Architect commit 988152c. Each test
//! carries a `// Python: <name>` line; `tests/ported_names.rs` checks the set.
//! Where the prototype test encodes behaviour the live `fleet-poll.py` no
//! longer has (the 45/50 ctx bar, the 5-minute load, the cwd-picked
//! transcript, the tm-supervisor skip), the port keeps the name and inputs and
//! asserts what the live script returns; the goldens in `tests/parity_a3.rs`
//! are the byte-level proof. The transcript-ctx cases run the pieces of
//! `cycle()` they exercise (parse, transcript fallback, evaluate); `cycle()`
//! itself is A3b. Every file lives in a tempdir.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use trusty_architect::classify::{
    Alert, DedupState, PaneState, REEMIT, dedup, evaluate, parse_pane,
};
use trusty_architect::poller::{
    BandConfig, Config, DiskUsage, HostProbe, SUPERVISOR, TranscriptCtx, band_alerts, builder_band,
    host_alerts, read_events, self_ctx_alerts,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn kinds(alerts: &[Alert]) -> Vec<&str> {
    let mut k: Vec<&str> = alerts.iter().map(|a| a.kind.as_str()).collect();
    k.sort_unstable();
    k
}

/// A `Config` with every path under `dir` and the env `vars` on top.
fn config(dir: &Path, vars: &[(&str, &str)]) -> Result<Config, trusty_architect::ConfigError> {
    let base: Vec<(String, String)> = [
        ("SUPERVISOR_INBOX_DIR", "inbox"),
        ("SESSIONS_DIR", "sessions"),
        ("TRANSCRIPT_BASE", "projects"),
        ("SELF_CTX_DIR", "self"),
    ]
    .iter()
    .map(|(k, sub)| {
        (
            (*k).to_owned(),
            dir.join(sub).to_string_lossy().into_owned(),
        )
    })
    .chain(vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
    .collect();
    let get = move |k: &str| {
        base.iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    Config::from_env(dir, dir, &get)
}

fn set_mtime(path: &Path, secs: u64) -> io::Result<()> {
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
}

fn usage_line(usage: &serde_json::Value, msg_type: &str, entrypoint: &str) -> String {
    serde_json::json!({"type": msg_type, "message": {"usage": usage}, "entrypoint": entrypoint})
        .to_string()
        + "\n"
}

fn assistant(usage: serde_json::Value) -> String {
    usage_line(&usage, "assistant", "cli")
}

// --- events ------------------------------------------------------------------

// Python: test_events_become_alerts_except_idle_prompt
#[test]
fn test_events_become_alerts_except_idle_prompt() -> TestResult {
    let dir = tempfile::tempdir()?;
    let ev = dir.path().join("events.jsonl");
    fs::write(&ev, "")?;
    let mut offset = None;
    assert!(read_events(&ev, &mut offset, 0.0)?.is_empty());
    assert_eq!(offset, Some(0));
    let row = |s: &str, p: Option<&str>, t: &str, m: &str| {
        let mut v = serde_json::json!({"tmux_session": s, "type": t, "message": m});
        if let Some(p) = p {
            v["tmux_pane"] = p.into();
        }
        v.to_string() + "\n"
    };
    let rows = [
        row(
            "tm-api",
            Some("%3"),
            "permission_prompt",
            "Claude needs your permission to use Bash",
        ),
        row(
            "tm-api",
            Some("%3"),
            "idle_prompt",
            "Claude is waiting for your input",
        ),
        row(
            "tm-api",
            Some("%3"),
            "agent_needs_input",
            "Agent needs input",
        ),
        row("tm-supervisor", None, "permission_prompt", "x"),
    ];
    fs::write(&ev, rows.concat())?;
    let got = read_events(&ev, &mut offset, 0.0)?;
    // The live script's SKIP_SESSIONS has no tm-supervisor (it names
    // tm-architect), so that row alerts too; the prototype test predates it.
    assert_eq!(
        kinds(&got),
        [
            "agent_needs_input",
            "permission_prompt",
            "permission_prompt"
        ]
    );
    assert!(
        got.iter()
            .any(|a| a.session == "tm-supervisor" && a.pane.is_empty())
    );
    assert!(read_events(&ev, &mut offset, 0.0)?.is_empty());
    let skipped = row(SUPERVISOR, Some("%1"), "permission_prompt", "own pane");
    fs::write(&ev, rows.concat() + &skipped)?;
    assert!(
        read_events(&ev, &mut offset, 0.0)?.is_empty(),
        "tm-architect events are dropped"
    );
    Ok(())
}

// --- self_ctx ------------------------------------------------------------------

fn self_ctx_cfg(
    dir: &Path,
    vars: &[(&str, &str)],
) -> Result<Config, trusty_architect::ConfigError> {
    let d = dir.to_string_lossy().into_owned();
    let mut all = vec![("SELF_CTX_DIR", d.as_str())];
    all.extend_from_slice(vars);
    config(dir, &all)
}

// Python: test_self_ctx_alerts_only_when_over_threshold
#[test]
fn test_self_ctx_alerts_only_when_over_threshold() -> TestResult {
    let dir = tempfile::tempdir()?;
    let cfg = self_ctx_cfg(dir.path(), &[])?;
    let over = dir.path().join("over.jsonl");
    let body = usage_line(&serde_json::json!({"input_tokens": 1}), "user", "cli")
        + &assistant(serde_json::json!({"input_tokens": 100_000,
            "cache_read_input_tokens": 300_000, "cache_creation_input_tokens": 5_000}));
    fs::write(&over, body)?;
    set_mtime(&over, 1000)?;
    let alerts = self_ctx_alerts(&cfg, 0.0)?;
    assert_eq!(kinds(&alerts), ["self_ctx"]);
    assert!(
        alerts[0].detail.contains("self ctx 40%"),
        "{}",
        alerts[0].detail
    );
    assert_eq!(alerts[0].session, SUPERVISOR);
    let under = dir.path().join("under.jsonl");
    fs::write(
        &under,
        assistant(serde_json::json!({"input_tokens": 1_000, "cache_read_input_tokens": 2_000})),
    )?;
    set_mtime(&under, 2000)?;
    assert!(self_ctx_alerts(&cfg, 0.0)?.is_empty());
    Ok(())
}

// Python: test_self_ctx_alerts_ignores_a_newer_sdk_cli_transcript
#[test]
fn test_self_ctx_alerts_ignores_a_newer_sdk_cli_transcript() -> TestResult {
    let dir = tempfile::tempdir()?;
    let cfg = self_ctx_cfg(dir.path(), &[])?;
    let sup = dir.path().join("supervisor.jsonl");
    fs::write(
        &sup,
        assistant(serde_json::json!({"input_tokens": 900_000})),
    )?;
    set_mtime(&sup, 1000)?;
    let col = dir.path().join("collector.jsonl");
    fs::write(
        &col,
        usage_line(
            &serde_json::json!({"input_tokens": 1}),
            "assistant",
            "sdk-cli",
        ),
    )?;
    set_mtime(&col, 9000)?;
    assert_eq!(kinds(&self_ctx_alerts(&cfg, 0.0)?), ["self_ctx"]);
    Ok(())
}

// Python: test_self_ctx_missing_or_empty_dir_never_crashes
#[test]
fn test_self_ctx_missing_or_empty_dir_never_crashes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("does-not-exist");
    let cfg = self_ctx_cfg(&missing, &[])?;
    assert!(self_ctx_alerts(&cfg, 0.0)?.is_empty());
    let cfg = self_ctx_cfg(dir.path(), &[])?;
    assert!(self_ctx_alerts(&cfg, 0.0)?.is_empty());
    Ok(())
}

// Python: test_self_ctx_window_and_threshold_are_env_overridable
#[test]
fn test_self_ctx_window_and_threshold_are_env_overridable() -> TestResult {
    let dir = tempfile::tempdir()?;
    fs::write(
        dir.path().join("t.jsonl"),
        assistant(serde_json::json!({"input_tokens": 50, "cache_read_input_tokens": 50})),
    )?;
    let cfg = self_ctx_cfg(
        dir.path(),
        &[("SELF_CTX_WINDOW", "1000"), ("SELF_CTX_THRESHOLD", "5")],
    )?;
    assert_eq!(kinds(&self_ctx_alerts(&cfg, 0.0)?), ["self_ctx"]);
    let cfg = self_ctx_cfg(
        dir.path(),
        &[("SELF_CTX_WINDOW", "1000"), ("SELF_CTX_THRESHOLD", "50")],
    )?;
    assert!(self_ctx_alerts(&cfg, 0.0)?.is_empty());
    Ok(())
}

// Python: test_self_ctx_reemit_interval_matches_ctx
#[test]
fn test_self_ctx_reemit_interval_matches_ctx() -> TestResult {
    let dir = tempfile::tempdir()?;
    fs::write(
        dir.path().join("t.jsonl"),
        assistant(serde_json::json!({"input_tokens": 500_000, "cache_read_input_tokens": 0})),
    )?;
    let cfg = self_ctx_cfg(dir.path(), &[])?;
    let interval = |k: &str| REEMIT.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    assert_eq!(interval("self_ctx"), interval("ctx"));
    let mut st = DedupState::default();
    let (a1, s1) = dedup(&mut st, &self_ctx_alerts(&cfg, 0.0)?, 0.0);
    assert!(a1.len() == 1 && s1 == 0);
    assert_eq!(
        dedup(&mut st, &self_ctx_alerts(&cfg, 1799.0)?, 1799.0),
        (vec![], 1)
    );
    assert_eq!(
        dedup(&mut st, &self_ctx_alerts(&cfg, 1800.0)?, 1800.0)
            .0
            .len(),
        1
    );
    Ok(())
}

// --- ctx from the pane's own transcript --------------------------------------

const RULE80: &str =
    "────────────────────────────────────────────────────────────────────────────────";

/// The prototype's `screen(PM_TURN, agents=["x"], ctx=ctx)`.
fn busy_screen(ctx: u32) -> String {
    let footer = format!(
        "  TM 1.7.4 ● | org/proj ⎇ tm-x | @me | Opus 5.5 | ctx {ctx}% | $1.00 | ⏳30% 📅40% | 💸1%/1%"
    );
    [
        "⏺ Merged #12 and the CI run is green.",
        "",
        "  Next: the release notes.",
        RULE80,
        "❯\u{a0}",
        RULE80,
        &footer,
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 9 agents",
        &format!("{}/rc", " ".repeat(70)),
        "",
        "  ⏺ main",
        "  ◯ x  Doing things…  3m 2s · ↓ 12.0k tokens",
    ]
    .join("\n")
        + "\n\n"
}

/// A live session-registry entry mapping pane `pane` to `sid`, and that
/// session's transcript holding `tokens` (`None` writes no transcript).
fn transcript_env(dir: &Path, pane: &str, sid: &str, tokens: Option<u64>) -> io::Result<()> {
    let sdir = dir.join("sessions");
    fs::create_dir_all(&sdir)?;
    let entry = serde_json::json!({"pid": std::process::id(), "sessionId": sid,
        "tmux": format!("tm-proj:@1.{pane}"), "updatedAt": 1});
    fs::write(sdir.join("1.json"), entry.to_string())?;
    if let Some(t) = tokens {
        let d = dir.join("projects/-w-proj");
        fs::create_dir_all(&d)?;
        fs::write(
            d.join(format!("{sid}.jsonl")),
            assistant(serde_json::json!({"input_tokens": 10, "cache_read_input_tokens": t - 10})),
        )?;
    }
    Ok(())
}

/// The A3a half of one `cycle()` pane step: parse, transcript fallback, evaluate.
fn ctx_cycle(
    cfg: &Config,
    ctx: &mut TranscriptCtx,
    cap: &str,
    pane: &str,
) -> TestResult<Vec<Alert>> {
    let mut info = parse_pane(cap);
    ctx.fill_ctx(cfg, &mut info, pane)?;
    Ok(evaluate(
        &info,
        &mut PaneState::default(),
        0.0,
        "tm-proj",
        pane,
    ))
}

/// Python's `re` `\d` matches every Unicode decimal digit and `int()` reads
/// it, so an Arabic-Indic `ctx 47%` is 47 (Architect ruling Q6). Red against
/// an ASCII-only `\d`.
#[test]
fn unicode_ctx_digits_parse_like_python() {
    let cap = busy_screen(30).replace("ctx 30%", "ctx \u{664}\u{667}%");
    assert_eq!(parse_pane(&cap).ctx, Some(47));
    let cap = busy_screen(30).replace("ctx 30%", "ctx \u{FF14}\u{FF17}%");
    assert_eq!(parse_pane(&cap).ctx, Some(47), "fullwidth");
}

// Python: test_status_line_ctx_wins_over_the_transcript
#[test]
fn test_status_line_ctx_wins_over_the_transcript() -> TestResult {
    let dir = tempfile::tempdir()?;
    transcript_env(dir.path(), "%40", "sid-p", Some(700_000))?;
    let cfg = config(dir.path(), &[])?;
    let mut ctx = TranscriptCtx::new();
    let alerts = ctx_cycle(&cfg, &mut ctx, &busy_screen(30), "%40")?;
    // The live bar (22) alerts on the status line's 30%, never the transcript's 70%.
    let shown: Vec<_> = alerts
        .iter()
        .map(|a| (a.kind.as_str(), a.detail.as_str()))
        .collect();
    assert_eq!(shown, [("ctx", "ctx 30%")]);
    assert_eq!(
        ctx.reads(),
        0,
        "the transcript is not read when the status line has ctx"
    );
    Ok(())
}

// Python: test_status_line_without_a_plain_ctx_percent_uses_the_transcript
#[test]
fn test_status_line_without_a_plain_ctx_percent_uses_the_transcript() -> TestResult {
    for replacement in ["⤵ 969k→77k (92%) |", "ctx>200k |"] {
        let dir = tempfile::tempdir()?;
        transcript_env(dir.path(), "%40", "sid-p", Some(699_000))?;
        let cfg = config(dir.path(), &[])?;
        let cap = busy_screen(30).replace("ctx 30% |", replacement);
        assert_eq!(parse_pane(&cap).ctx, None, "{replacement}");
        let alerts = ctx_cycle(&cfg, &mut TranscriptCtx::new(), &cap, "%40")?;
        let shown: Vec<_> = alerts
            .iter()
            .map(|a| (a.kind.as_str(), a.fingerprint.as_str(), a.detail.as_str()))
            .collect();
        assert_eq!(shown, [("ctx", "25", "ctx 69%")], "{replacement}");
    }
    Ok(())
}

// Python: test_no_transcript_means_no_ctx_alert_and_no_crash
#[test]
fn test_no_transcript_means_no_ctx_alert_and_no_crash() -> TestResult {
    let dir = tempfile::tempdir()?;
    transcript_env(dir.path(), "%40", "sid-p", None)?;
    let cfg = config(dir.path(), &[])?;
    let cap = busy_screen(30).replace("ctx 30% |", "⤵ 969k→77k (92%) |");
    let mut ctx = TranscriptCtx::new();
    assert!(ctx_cycle(&cfg, &mut ctx, &cap, "%40")?.is_empty());
    assert_eq!(ctx.pct(&cfg, "")?, None);
    Ok(())
}

// Python: test_transcript_ctx_is_cached_until_the_file_changes
#[test]
fn test_transcript_ctx_is_cached_until_the_file_changes() -> TestResult {
    let dir = tempfile::tempdir()?;
    transcript_env(dir.path(), "%40", "sid-p", Some(460_000))?;
    let cfg = config(dir.path(), &[])?;
    let mut ctx = TranscriptCtx::new();
    assert_eq!(ctx.pct(&cfg, "%40")?, Some(46));
    assert_eq!(ctx.pct(&cfg, "%40")?, Some(46));
    assert_eq!(ctx.reads(), 1);
    let f = dir.path().join("projects/-w-proj/sid-p.jsonl");
    let grown = fs::read_to_string(&f)? + &assistant(serde_json::json!({"input_tokens": 520_000}));
    fs::write(&f, grown)?;
    assert_eq!((ctx.pct(&cfg, "%40")?, ctx.reads()), (Some(52), 2));
    Ok(())
}

/// The cache key is `(mtime, size)`: a change to either alone re-reads. Red
/// against a key of the path alone, of mtime alone, or of size alone.
#[test]
fn transcript_cache_key_is_mtime_and_size() -> TestResult {
    let dir = tempfile::tempdir()?;
    transcript_env(dir.path(), "%40", "sid-p", Some(460_000))?;
    let cfg = config(dir.path(), &[])?;
    let f = dir.path().join("projects/-w-proj/sid-p.jsonl");
    set_mtime(&f, 5_000)?;
    let mut ctx = TranscriptCtx::new();
    assert_eq!(ctx.pct(&cfg, "%40")?, Some(46));
    // Same size, new mtime: the text changes length-for-length.
    fs::write(
        &f,
        assistant(serde_json::json!({"input_tokens": 10, "cache_read_input_tokens": 470_000 - 10})),
    )?;
    set_mtime(&f, 6_000)?;
    assert_eq!((ctx.pct(&cfg, "%40")?, ctx.reads()), (Some(47), 2));
    // Same mtime, new size.
    fs::write(
        &f,
        assistant(
            serde_json::json!({"input_tokens": 10, "cache_read_input_tokens": 4_800_000 - 10}),
        ),
    )?;
    set_mtime(&f, 6_000)?;
    assert_eq!((ctx.pct(&cfg, "%40")?, ctx.reads()), (Some(480), 3));
    assert_eq!((ctx.pct(&cfg, "%40")?, ctx.reads()), (Some(480), 3));
    Ok(())
}

// Python: test_same_cwd_panes_get_their_own_ctx
#[test]
fn test_same_cwd_panes_get_their_own_ctx() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (base, sdir) = (
        dir.path().join("projects/-Users-x-trusty-tools"),
        dir.path().join("sessions"),
    );
    fs::create_dir_all(&base)?;
    fs::create_dir_all(&sdir)?;
    for (sid, tokens, mtime) in [("sid-a", 450_000, 2_000), ("sid-b", 180_000, 1_000)] {
        let p = base.join(format!("{sid}.jsonl"));
        fs::write(&p, assistant(serde_json::json!({"input_tokens": tokens})))?;
        set_mtime(&p, mtime)?;
    }
    let session = |pid: u64, sid: &str, pane: &str| {
        serde_json::json!({"pid": pid, "sessionId": sid, "tmux": format!("tm-x:@1.{pane}"), "updatedAt": 1})
            .to_string()
    };
    let me = u64::from(std::process::id());
    fs::write(sdir.join(format!("{me}.json")), session(me, "sid-a", "%1"))?;
    fs::write(
        sdir.join("dead.json"),
        session(2_147_483_000, "sid-b", "%2"),
    )?;
    let cfg = config(dir.path(), &[])?;
    let mut ctx = TranscriptCtx::new();
    assert_eq!(ctx.pct(&cfg, "%1")?, Some(45));
    assert_eq!(ctx.pct(&cfg, "%2")?, None, "unresolved: no borrowed value");
    fs::write(sdir.join("1.json"), session(1, "sid-b", "%2"))?; // pid 1 is always alive
    assert_eq!(ctx.pct(&cfg, "%2")?, Some(18));
    assert_eq!(ctx.pct(&cfg, "%9")?, None);
    Ok(())
}

// --- load, disk, swap and the builder band -----------------------------------

/// Injected host values: load averages, cores, and a calm disk/swap/memory.
struct Probe {
    loads: [f64; 3],
    cores: usize,
    free: u64,
}

impl Probe {
    fn new(loads: [f64; 3], cores: usize) -> Self {
        Self {
            loads,
            cores,
            free: 500 << 30,
        }
    }
}

impl HostProbe for Probe {
    fn loadavg(&self) -> io::Result<[f64; 3]> {
        Ok(self.loads)
    }
    fn cores(&self) -> usize {
        self.cores
    }
    fn data_volume(&self) -> std::path::PathBuf {
        "/data".into()
    }
    fn disk_usage(&self, _: &Path) -> io::Result<DiskUsage> {
        Ok(DiskUsage {
            total: 100,
            used: 1,
            free: self.free,
        })
    }
    fn swap_used_mb(&self) -> io::Result<Option<f64>> {
        Ok(Some(0.0))
    }
    fn mem_free_pct(&self) -> Option<f64> {
        Some(60.0)
    }
}

fn load_alerts(probe: &Probe, factor: Option<&str>) -> TestResult<Vec<Alert>> {
    let dir = tempfile::tempdir()?;
    let vars: Vec<(&str, &str)> = factor.map(|f| ("LOAD_FACTOR", f)).into_iter().collect();
    let cfg = config(dir.path(), &vars)?;
    let all = host_alerts(probe, &cfg, 0.0, None)?;
    Ok(all.into_iter().filter(|a| a.kind == "load").collect())
}

// Python: test_a_brief_1min_spike_with_a_low_5min_average_does_not_alert
#[test]
fn test_a_brief_1min_spike_with_a_low_5min_average_does_not_alert() -> TestResult {
    assert!(load_alerts(&Probe::new([29.7, 16.8, 10.0], 16), None)?.is_empty());
    Ok(())
}

// Python: test_a_sustained_5min_average_above_1_5x_cores_alerts
#[test]
fn test_a_sustained_5min_average_above_1_5x_cores_alerts() -> TestResult {
    // The live script reads the 15-minute average: the prototype test's
    // 30/25/20 (5-minute 25 > 24) no longer alerts; a 15-minute 25 does.
    assert!(load_alerts(&Probe::new([30.0, 25.0, 20.0], 16), None)?.is_empty());
    let alerts = load_alerts(&Probe::new([30.0, 25.0, 24.5], 16), None)?;
    assert_eq!(alerts.len(), 1);
    assert!(
        alerts[0].detail.contains("30.0/25.0/24.5"),
        "{}",
        alerts[0].detail
    );
    assert!(
        load_alerts(&Probe::new([30.0, 25.0, 24.0], 16), None)?.is_empty(),
        "24 is not > 1.5x16"
    );
    Ok(())
}

// Python: test_load_factor_is_env_overridable
#[test]
fn test_load_factor_is_env_overridable() -> TestResult {
    assert_eq!(
        load_alerts(&Probe::new([10.0, 17.0, 17.0], 8), Some("2.0"))?.len(),
        1
    );
    assert!(load_alerts(&Probe::new([10.0, 17.0, 17.0], 8), Some("3.0"))?.is_empty());
    // The prototype test's 15-minute 5.0 is under either line.
    assert!(load_alerts(&Probe::new([10.0, 17.0, 5.0], 8), Some("2.0"))?.is_empty());
    Ok(())
}

// Python: test_load_reemit_interval_is_unchanged_at_1800s
#[test]
fn test_load_reemit_interval_is_unchanged_at_1800s() -> TestResult {
    assert_eq!(
        REEMIT.iter().find(|(k, _)| *k == "load").map(|(_, v)| *v),
        Some(1800.0)
    );
    let alerts = load_alerts(&Probe::new([30.0, 30.0, 30.0], 16), None)?;
    let mut st = DedupState::default();
    assert_eq!(dedup(&mut st, &alerts, 0.0).0.len(), 1);
    assert_eq!(dedup(&mut st, &alerts, 1799.0), (vec![], 1));
    assert_eq!(dedup(&mut st, &alerts, 1800.0).0.len(), 1);
    Ok(())
}

// Python: test_builder_band_boundaries
#[test]
fn test_builder_band_boundaries() {
    let cases: [(f64, f64, Option<f64>, f64, &str); 11] = [
        (0.0, 300.0, Some(70.0), 0.0, "open"),
        (17.9, 200.0, Some(20.0), 0.0, "open"),
        (18.0, 300.0, Some(70.0), 0.0, "hold"),
        (0.0, 199.0, Some(70.0), 0.0, "hold"),
        (30.0, 300.0, Some(70.0), 0.0, "hold"),
        (30.1, 300.0, Some(70.0), 0.0, "stop"),
        (0.0, 99.0, Some(70.0), 0.0, "stop"),
        (0.0, 100.0, Some(70.0), 0.0, "hold"),
        (0.0, 300.0, Some(19.0), 0.0, "stop"),
        (0.0, 300.0, None, 0.0, "open"),
        (0.0, 300.0, Some(70.0), 1.0, "stop"),
    ];
    let cfg = BandConfig::default();
    for (load, free, mem, swap, want) in cases {
        let got = builder_band(&cfg, load, 16, free, mem, Some(swap), None);
        assert_eq!(
            got.as_str(),
            want,
            "load {load} free {free} mem {mem:?} swap {swap}"
        );
    }
}

// Python: test_band_alert_fires_only_on_change_and_first_run
#[test]
fn test_band_alert_fires_only_on_change_and_first_run() {
    let cfg = BandConfig::default();
    let mut st = None;
    let first = band_alerts(&cfg, &mut st, 0.0, 1.0, 16, 300.0, Some(70.0), Some(0.0));
    assert_eq!(kinds(&first), ["band"]);
    assert_eq!(st.as_deref(), Some("open"));
    assert!(band_alerts(&cfg, &mut st, 1.0, 1.0, 16, 300.0, Some(70.0), Some(0.0)).is_empty());
    let chg = band_alerts(&cfg, &mut st, 2.0, 13.2, 16, 180.0, Some(70.0), Some(0.0));
    assert_eq!(st.as_deref(), Some("hold"));
    // The live detail names load15 (the prototype test predates the switch).
    assert_eq!(
        chg[0].detail,
        "builder band open -> hold (load15 13.2/16 cores, 180 GiB free, mem 70% free, swap 0)"
    );
}

// Python: test_builder_band_hysteresis
#[test]
fn test_builder_band_hysteresis() {
    let cases = [
        (15.5, "open", "open"),
        (18.0, "open", "open"),
        (20.0, "open", "open"),
        (20.5, "open", "hold"),
        (18.0, "hold", "hold"),
        (16.0, "hold", "hold"),
        (15.9, "hold", "open"),
        (15.9, "stop", "open"),
        (18.0, "stop", "hold"),
        (30.5, "open", "stop"),
    ];
    let cfg = BandConfig::default();
    for (load, prev, want) in cases {
        let got = builder_band(&cfg, load, 16, 300.0, Some(70.0), Some(0.0), Some(prev));
        assert_eq!(got.as_str(), want, "load {load} prev {prev}");
    }
}

// Python: test_builder_band_hysteresis_disk_still_gates_open
#[test]
fn test_builder_band_hysteresis_disk_still_gates_open() {
    let cfg = BandConfig::default();
    for prev in ["open", "hold"] {
        let got = builder_band(&cfg, 5.0, 16, 150.0, Some(70.0), Some(0.0), Some(prev));
        assert_eq!(got.as_str(), "hold", "{prev}");
    }
}

/// `scripts/test_fleet_poll_band.py`'s `_host`: `host_alerts` with a stored band.
fn host_with_band(loads: [f64; 3], band: &str) -> TestResult<(Vec<Alert>, Option<String>)> {
    let dir = tempfile::tempdir()?;
    let cfg = config(dir.path(), &[])?;
    let mut stored = Some(band.to_owned());
    let out = host_alerts(&Probe::new(loads, 16), &cfg, 0.0, Some(&mut stored))?;
    Ok((out, stored))
}

// Python: test_five_minute_spike_with_calm_15m_keeps_hold
#[test]
fn test_five_minute_spike_with_calm_15m_keeps_hold() -> TestResult {
    let (out, band) = host_with_band([30.0, 31.0, 20.0], "hold")?;
    assert_eq!(band.as_deref(), Some("hold"));
    assert!(out.iter().all(|a| a.kind != "band" && a.kind != "load"));
    Ok(())
}

// Python: test_high_15m_load_stops_and_names_load15
#[test]
fn test_high_15m_load_stops_and_names_load15() -> TestResult {
    let (out, band) = host_with_band([30.0, 31.0, 31.0], "hold")?;
    let bands: Vec<&Alert> = out.iter().filter(|a| a.kind == "band").collect();
    assert_eq!(band.as_deref(), Some("stop"));
    assert!(bands[0].detail.contains("load15"), "{}", bands[0].detail);
    Ok(())
}
