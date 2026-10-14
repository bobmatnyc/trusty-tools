//! #9801 slice 1: the usage step inside a whole poll cycle. Every file lives
//! in a tempdir; tmux is scripted; the probe launcher records instead of
//! typing into a pane.

mod cycle_support;

use std::fs;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use serde_json::json;
use trusty_architect::poller::steps::notify_macos::RecordingNotifier;
use trusty_architect::poller::steps::usage::{RecordingProbe, UtcTime};
use trusty_architect::poller::{PollSession, Runner, WakeAll, load_state};
use trusty_architect::{Clock, FakeClock};

const GM: &str = "a2@example.com";

/// Writes the account file and a ledger whose current account owes a
/// `wk>=80` and a `cutover` alert, at the config's default usage paths.
fn usage_files(env: &Env) -> TestResult {
    let usage = &env.config.usage;
    fs::create_dir_all(usage.claude_json.parent().ok_or("claude.json dir")?)?;
    fs::write(&usage.claude_json, json!({"oauthAccount": {"emailAddress": GM}}).to_string())?;
    fs::create_dir_all(usage.ledger.parent().ok_or("ledger dir")?)?;
    let rows = [
        json!({"ts": "2026-10-07T14:30:00Z", "account": GM, "pct5h": 1, "reset5h": "2026-10-07T20:50:00Z",
               "pctwk": 60, "resetwk": "2026-10-09T07:00:00Z", "source": "manual"}),
        json!({"ts": "2026-10-07T16:12:00Z", "account": GM, "pct5h": 9, "reset5h": "2026-10-07T20:50:00Z",
               "pctwk": 84, "resetwk": "2026-10-09T07:00:00Z", "source": "manual"}),
        json!({"ts": "2026-10-07T16:18:00Z", "account": "next@x", "pct5h": 0, "reset5h": null,
               "pctwk": 50, "resetwk": "2026-10-12T11:00:00Z", "source": "manual"}),
    ];
    fs::write(&usage.ledger, rows.iter().map(|r| format!("{r}\n")).collect::<String>())?;
    Ok(())
}

fn clock() -> TestResult<FakeClock> {
    Ok(FakeClock::new(UtcTime::parse_iso("2026-10-07T16:20:00Z")?.timestamp()))
}

/// The defaults the cycle reads: `<root>/local/usage/ledger.jsonl`,
/// `<inbox>/statusline-usage.json`, the claude-config `.claude.json`,
/// `<root>/scripts/usage-probe.sh` and `<inbox>/usage-probe.log`.
#[test]
fn usage_paths_default_like_the_prototype() -> TestResult {
    let env = Env::new(&[])?;
    let (root, u) = (env.root(), &env.config.usage);
    assert_eq!(u.ledger, root.join("local/usage/ledger.jsonl"));
    assert_eq!(u.cache, env.config.inbox.join("statusline-usage.json"));
    assert_eq!(
        u.claude_json,
        root.join(".trusty-tools/trusty-mpm/claude-config/.claude.json")
    );
    assert_eq!(u.probe_script, root.join("scripts/usage-probe.sh"));
    assert_eq!(u.probe_log, env.config.inbox.join("usage-probe.log"));
    let set = Env::new(&[
        ("USAGE_LEDGER", "/l.jsonl"),
        ("USAGE_STATUSLINE_CACHE", "/c.json"),
        ("CLAUDE_CONFIG_DIR", "/cfg"),
    ])?;
    assert_eq!(set.config.usage.ledger, std::path::Path::new("/l.jsonl"));
    assert_eq!(set.config.usage.cache, std::path::Path::new("/c.json"));
    assert_eq!(
        set.config.usage.claude_json,
        std::path::Path::new("/cfg/.claude.json")
    );
    let file = Env::new(&[("USAGE_CLAUDE_JSON", "/a.json"), ("CLAUDE_CONFIG_DIR", "/cfg")])?;
    assert_eq!(file.config.usage.claude_json, std::path::Path::new("/a.json"));
    Ok(())
}

/// One cycle: the usage alerts land in `alerts.jsonl`, the probe goes to the
/// one idle pane, and the saved state records the probe.
#[test]
fn usage_alerts_reach_alerts_jsonl_and_the_probe_runs_once() -> TestResult {
    let env = Env::new(&[])?;
    usage_files(&env)?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(10));
    let clock = clock()?;
    let runner = env.runner(&tmux, &clock, &QuietHost);
    let (mut session, _) = PollSession::load(&env.config, false, clock.now_unix());
    let summary = runner.run_once(&mut session)?;
    let usage: Vec<&str> = summary
        .new
        .iter()
        .filter(|a| a.kind == "usage")
        .map(|a| a.fingerprint.as_str())
        .collect();
    assert_eq!(
        usage,
        [format!("wk>=80:{GM}:2026-10-09T07:00:00Z"), format!("cutover:{GM}:next@x")]
    );
    assert!(env.keys()?.iter().any(|k| k.contains("\"usage\"")), "{:?}", env.keys()?);
    assert_eq!(env.probe.launched(), [["tm-a", "%2", GM].map(str::to_owned)]);
    let (state, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    assert_eq!(state.other["usage"]["tries"], json!(1));
    assert_eq!(state.other["usage"]["last_probe"], json!(clock.now_unix()));
    Ok(())
}

/// #9801 Fail-Open Check: a probe that cannot start is one log line; the
/// cycle still writes its alerts and saves its state, and the state does not
/// record a probe, so the next cycle tries again.
#[test]
fn a_failing_usage_probe_never_stops_the_cycle() -> TestResult {
    let env = Env::new(&[])?;
    usage_files(&env)?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(10));
    let clock = clock()?;
    let probe = RecordingProbe::failing();
    let notifier = RecordingNotifier::default();
    let runner = Runner {
        tmux: &tmux,
        clock: &clock,
        probe: &QuietHost,
        triage: &WakeAll,
        config: &env.config,
        usage_probe: &probe,
        notifier: &notifier,
        dry_run: false,
    };
    let (mut session, _) = PollSession::load(&env.config, false, clock.now_unix());
    let summary = runner.run_once(&mut session)?;
    assert!(summary.new.iter().any(|a| a.kind == "usage"), "{:?}", summary.new);
    assert!(env.log().contains("usage probe ERROR "), "{}", env.log());
    assert_eq!(probe.launched().len(), 1);
    let (state, err) = load_state(&env.state_path());
    assert!(err.is_none(), "{err:?}");
    assert_eq!(state.other["usage"].get("last_probe"), None);
    assert_eq!(state.other["usage"]["tries"], json!(0));
    assert!(notifier.sent().is_empty(), "no architect_* alert, no notification");
    Ok(())
}
