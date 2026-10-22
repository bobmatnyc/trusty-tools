//! #9822 (#9171 A4): `tm architect poll`, the port of `fleet-poll.py`'s
//! command line: its dry-run pane lines against the Python's, its
//! environment names, and the single run that fails where the script logs
//! and exits 0.
//!
//! Every path lives under a tempdir; tmux is scripted; nothing reaches a live
//! server or inbox.

mod cycle_support;

use std::fs;
use std::path::Path;

use cycle_support::{Env, QuietHost, TestResult, fleet, screen};
use trusty_architect::poller::steps::usage::RecordingProbe;
use trusty_architect::poller::{PollState, RunError, Sensors, TranscriptCtx, Waker, collect};
use trusty_architect::verbs::{self, PollArgs, VerbError};
use trusty_architect::{FakeClock, ScriptedTmux};

/// `regen_a4.py`'s fixed clock.
const NOW: f64 = 1_760_290_860.0;

fn a4() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a4")
}

/// The fleet in `panes/list-panes.txt`, each pane capturing its fixture.
fn fixture_fleet() -> Result<ScriptedTmux, Box<dyn std::error::Error>> {
    let dir = a4().join("panes");
    let targets = fs::read_to_string(dir.join("targets.tsv"))?;
    let listing = fs::read_to_string(dir.join("list-panes.txt"))?;
    let mut tmux = ScriptedTmux::new()
        .script("list-panes", &listing)
        .fallback("list-windows", "");
    for row in listing.lines() {
        let mut cols = row.split('\t');
        let (session, pane) = (
            cols.next().unwrap_or_default(),
            cols.next().unwrap_or_default(),
        );
        if session == "tm-architect-poll" {
            continue; // a skipped session: never captured
        }
        let file = targets
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{pane}\t")))
            .ok_or("pane missing from targets.tsv")?;
        tmux = tmux.script("capture-pane", &fs::read_to_string(dir.join(file))?);
    }
    Ok(tmux)
}

#[test]
fn dry_run_pane_lines_match_python() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fixture_fleet()?;
    let sensors = Sensors {
        tmux: &tmux,
        probe: &QuietHost,
        config: &env.config,
        usage_probe: &RecordingProbe::default(),
        dry_run: true,
    };
    let found = collect(
        sensors,
        &mut TranscriptCtx::default(),
        &mut PollState::default(),
        NOW,
    )
    .map_err(|e| format!("{e:?}"))?;
    let got: String = found
        .log
        .iter()
        .filter(|l| l.starts_with("  "))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(got, fs::read_to_string(a4().join("poll_dry_panes.txt"))?);
    Ok(())
}

#[test]
fn run_single_fails_closed_when_tmux_cannot_list_panes() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = ScriptedTmux::new().fail("list-panes", "no server running");
    let clock = FakeClock::new(NOW);
    let err = env
        .runner(&tmux, &clock, &QuietHost)
        .run_single("60")
        .expect_err("a skipped cycle must fail a single run");
    assert!(matches!(err, RunError::Incomplete(_)), "{err}");
    assert!(err.to_string().contains("no server running"), "{err}");
    assert!(clock.sleeps().is_empty(), "--once never sleeps");
    Ok(())
}

#[test]
fn run_single_fails_closed_when_the_state_cannot_be_read() -> TestResult {
    let env = Env::new(&[])?;
    fs::create_dir_all(env.state_path())?; // a directory: the read fails
    let tmux = fleet(&[("tm-a", "%1", "claude")], &screen(30));
    let clock = FakeClock::new(NOW);
    let err = env
        .runner(&tmux, &clock, &QuietHost)
        .run_single("60")
        .expect_err("an unsaved state must fail a single run");
    assert!(matches!(err, RunError::Incomplete(_)), "{err}");
    Ok(())
}

/// #9822 criterion 5: a single run with `var` set to `value` returns an error
/// that names `var`, in a real run and in a dry run, and does not sleep.
fn single_run_rejects(var: &str, value: &str) -> TestResult {
    for dry_run in [false, true] {
        let env = Env::new(&[(var, value)])?;
        let tmux = fleet(&[("tm-a", "%1", "claude")], &screen(30));
        let clock = FakeClock::new(NOW);
        let mut runner = env.runner(&tmux, &clock, &QuietHost);
        runner.dry_run = dry_run;
        let err = runner
            .run_single("60")
            .expect_err("a bad variable must fail a single run");
        assert!(matches!(err, RunError::BadConfig(_)), "{err}");
        assert!(err.to_string().contains(var), "dry_run={dry_run}: {err}");
        assert!(clock.sleeps().is_empty(), "--once never sleeps");
    }
    Ok(())
}

#[test]
fn run_single_fails_closed_on_a_bad_self_ctx_threshold() -> TestResult {
    single_run_rejects("SELF_CTX_THRESHOLD", "25%")
}

#[test]
fn run_single_fails_closed_on_a_bad_load_factor() -> TestResult {
    single_run_rejects("LOAD_FACTOR", "lots")
}

#[test]
fn run_single_fails_closed_on_a_bad_pane_ctx_window() -> TestResult {
    single_run_rejects("PANE_CTX_WINDOW", "")
}

#[test]
fn run_single_fails_closed_on_a_bad_self_ctx_window() -> TestResult {
    single_run_rejects("SELF_CTX_WINDOW", "1e6")
}

#[test]
fn run_single_runs_one_cycle_and_saves() -> TestResult {
    let env = Env::new(&[])?;
    let tmux = fleet(&[("tm-a", "%1", "claude")], &screen(30));
    let clock = FakeClock::new(NOW);
    let summary = env.runner(&tmux, &clock, &QuietHost).run_single("60")?;
    assert!(summary.wake.is_some() && summary.skipped.is_none());
    assert!(env.state_path().is_file(), "the state was saved");
    assert!(
        env.log().contains("poll start interval=60s"),
        "{}",
        env.log()
    );
    assert!(clock.sleeps().is_empty());
    Ok(())
}

#[test]
fn poll_config_reads_the_shipped_names() -> TestResult {
    let root = Path::new("/arch");
    let home = Path::new("/home/u");
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    };
    let c = verbs::poll_config(root, home, &vars(&[]))?;
    assert_eq!(c.inbox, Path::new("/arch/inbox"));
    assert_eq!(
        c.quiet_sessions,
        Path::new("/arch/local/quiet-sessions.txt")
    );

    let shipped = vars(&[
        ("ARCHITECT_INBOX_DIR", "/q/inbox"),
        ("ARCHITECT_QUIET_SESSIONS_FILE", "/q/quiet.txt"),
        ("ARCHITECT_SESSION", "tm-architect"),
        ("ARCHITECT_POLL_SESSION", "tm-architect-poll"),
    ]);
    let c = verbs::poll_config(root, home, &shipped)?;
    assert_eq!(c.inbox, Path::new("/q/inbox"));
    assert_eq!(c.quiet_sessions, Path::new("/q/quiet.txt"));

    assert_eq!(
        (c.supervisor.as_str(), c.poll_session.as_str()),
        ("tm-architect", "tm-architect-poll")
    );

    // `tm fleet init --session tm-sup-x` sets the session; the poll session follows.
    let custom = verbs::poll_config(root, home, &vars(&[("ARCHITECT_SESSION", "tm-sup-x")]))?;
    assert_eq!(
        (custom.supervisor.as_str(), custom.poll_session.as_str()),
        ("tm-sup-x", "tm-sup-x-poll")
    );

    for bad in [
        vars(&[("ARCHITECT_SESSION", "tm-a:0")]),
        vars(&[("ARCHITECT_POLL_SESSION", "tm a")]),
    ] {
        let err = verbs::poll_config(root, home, &bad).expect_err("not a session name");
        assert!(matches!(err, VerbError::Usage(_)), "{err}");
        assert_eq!(err.exit_code(), 2);
    }
    Ok(())
}

/// #9822: `fleet-poll.py` reads only `ARCHITECT_INBOX_DIR`, so it wins over
/// `SUPERVISOR_INBOX_DIR` when both are set.
#[test]
fn architect_inbox_dir_outranks_supervisor_inbox_dir() -> TestResult {
    let both = |k: &str| match k {
        "ARCHITECT_INBOX_DIR" => Some("/q/inbox".to_owned()),
        "SUPERVISOR_INBOX_DIR" => Some("/s/inbox".to_owned()),
        _ => None,
    };
    let c = verbs::poll_config(Path::new("/arch"), Path::new("/home/u"), &both)?;
    assert_eq!(c.inbox, Path::new("/q/inbox"));
    Ok(())
}

/// #9822: with `ARCHITECT_SESSION=tm-sup-x` the cycle watches neither the
/// supervisor nor its poller, drops their events, and wakes only `tm-sup-x`,
/// never a `tm-architect` pane that is also listed.
#[test]
fn a_configured_supervisor_is_skipped_and_the_only_pane_woken() -> TestResult {
    let env = Env::new(&[])?;
    let vars = |k: &str| (k == "ARCHITECT_SESSION").then(|| "tm-sup-x".to_owned());
    let mut config = verbs::poll_config(env.root(), env.root(), &vars)?;
    config.inbox = env.config.inbox.clone();
    config.sessions_dir = env.config.sessions_dir.clone();
    config.transcript_base = env.config.transcript_base.clone();
    config.self_ctx_dir = env.config.self_ctx_dir.clone();
    let event = |session: &str, pane: &str| {
        format!(
            "{{\"type\": \"permission_prompt\", \"tmux_session\": \"{session}\", \"tmux_pane\": \"{pane}\"}}\n"
        )
    };
    fs::write(
        config.events(),
        event("tm-sup-x", "%5") + &event("tm-a", "%7"),
    )?;
    let rows = [
        ("tm-architect", "%1", "claude"),
        ("tm-sup-x", "%5", "claude"),
        ("tm-sup-x-poll", "%6", "python3"),
        ("tm-a", "%7", "claude"),
    ];
    let tmux = fleet(&rows, &screen(30));
    let sensors = Sensors {
        tmux: &tmux,
        probe: &QuietHost,
        config: &config,
        usage_probe: &RecordingProbe::default(),
        dry_run: true,
    };
    let mut state = PollState {
        events_offset: Some(0),
        ..PollState::default()
    };
    let found = collect(sensors, &mut TranscriptCtx::default(), &mut state, NOW)
        .map_err(|e| format!("{e:?}"))?;
    let events = |session: &str| {
        found
            .new
            .iter()
            .filter(|a| a.session == session && a.kind == "permission_prompt")
            .count()
    };
    assert_eq!(
        events("tm-a"),
        1,
        "the control event alerts: {:?}",
        found.new
    );
    // Only tm-a: tm-architect stays in SKIP_SESSIONS, and tm-sup-x and its
    // poller are this config's skips.
    assert_eq!(found.watched, 1, "{:?}", found.log);
    assert!(
        found
            .new
            .iter()
            .all(|a| a.session != "tm-sup-x" && a.session != "tm-sup-x-poll"),
        "{:?}",
        found.new
    );

    let clock = FakeClock::new(NOW);
    let alerts = config.alerts();
    let waker = Waker {
        tmux: &tmux,
        clock: &clock,
        alerts: &alerts,
        dry_run: true,
        messages: &trusty_architect::poller::NoMessages,
    };
    let phrase = waker.dispatch_as(
        &config.supervisor,
        &mut PollState::default(),
        &found.panes,
        1,
        None,
    );
    assert!(phrase.contains("would send to %5"), "{phrase}");
    let captured: Vec<_> = tmux
        .calls()
        .into_iter()
        .filter(|c| c[0] == "capture-pane")
        .collect();
    assert!(
        captured
            .last()
            .is_some_and(|c| c.contains(&"%5".to_owned())),
        "{captured:?}"
    );
    Ok(())
}

/// argparse's `type=int` refuses these with exit 2 (`poll_args.jsonl`); the
/// port refuses them before any tmux call.
#[test]
fn a_bad_interval_is_a_usage_error() -> TestResult {
    let env = Env::new(&[])?;
    for interval in ["x", "1.5", "0", "-5", ""] {
        let args = PollArgs {
            once: true,
            interval: interval.to_owned(),
            dry_run: false,
        };
        let err = verbs::poll(&args, &env.config, &trusty_architect::poller::NoMessages)
            .expect_err(interval);
        assert_eq!(err.exit_code(), 2, "{interval}: {err}");
    }
    Ok(())
}
