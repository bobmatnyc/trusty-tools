//! Tests for the Telegram gateway status surface (#8190).
//!
//! Why: the whole point of this module is that a gateway degradation is
//! REPORTABLE on a host that captures no stderr, so each test asserts on what
//! `tagent system status` would render rather than on a log line. The state is
//! a process global, so every test here is serialized against the others.
//! Test: this module is itself the coverage.

use super::*;

/// A fixture token, used only to prove it never reaches the output.
const FIXTURE_TOKEN: &str = "7427:AAH-fixture-bot-token-never-rendered";

fn row(refs: &str, state: &str) -> TelegramBotStatus {
    TelegramBotStatus {
        credential_refs: refs.to_string(),
        assistants: vec!["izzie".to_string()],
        state: state.to_string(),
        detail: None,
    }
}

/// A binding whose token will not resolve is visible in `system status`, with
/// its reason — the surface that replaces a log line izzie's host never keeps.
#[test]
#[serial_test::serial]
fn telegram_gateway_status_records_a_skipped_binding() {
    clear();
    record_scan(
        vec![TelegramBotStatus {
            credential_refs: "tg-ghost".into(),
            assistants: vec!["ghost".into()],
            state: "skipped".into(),
            detail: Some("no credential stored for `telegram/missing`".into()),
        }],
        vec!["the assistant roster could not be read".into()],
    );
    let snapshot = snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"));
    assert_eq!(snapshot.bots.len(), 1);
    assert_eq!(snapshot.bots[0].state, "skipped");
    assert!(
        snapshot.bots[0]
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("telegram/missing")),
        "the skip reason must survive into the report: {:?}",
        snapshot.bots[0]
    );
    assert_eq!(snapshot.warnings.len(), 1);
    clear();
}

/// #8190 finding 4: the scan re-runs, so a fixed binding must be able to remove
/// its own warning. Appending would grow a log of stale complaints.
#[test]
#[serial_test::serial]
fn telegram_gateway_status_scan_replaces_the_previous_warnings() {
    clear();
    record_scan(vec![row("telegram", "skipped")], vec!["stale".into()]);
    record_scan(vec![row("telegram", "polling")], Vec::new());
    let snapshot = snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"));
    assert!(snapshot.warnings.is_empty(), "{:?}", snapshot.warnings);
    assert_eq!(snapshot.bots[0].state, "polling");
    clear();
}

/// A live state change between scans reaches the report.
#[test]
#[serial_test::serial]
fn telegram_gateway_status_records_a_live_state_change() {
    clear();
    record_scan(vec![row("telegram/izzie", "starting")], Vec::new());
    record_state(
        "telegram/izzie",
        "waiting-for-lock",
        Some("PID 4242 holds this bot's gateway lock".into()),
    );
    let snapshot = snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"));
    assert_eq!(snapshot.bots[0].state, "waiting-for-lock");
    assert!(
        snapshot.bots[0]
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("4242"))
    );
    // A bot the scan never reported is not invented.
    record_state("telegram/unknown", "polling", None);
    assert_eq!(
        snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"))
            .bots
            .len(),
        1
    );
    clear();
}

/// #8190 round-2 finding 4: every rescan rewrote each bot's row with the
/// `starting` placeholder, while `supervise_bot` records `polling` only at a
/// TRANSITION — so a healthy poller read `starting` forever after the first
/// rescan, and the one surface an operator can consult said the gateway was
/// permanently coming up.
///
/// Pre-change this fails: the second `record_scan` overwrites the live row and
/// the state reads `starting`.
#[test]
#[serial_test::serial]
fn telegram_gateway_status_a_rescan_preserves_a_polling_row() {
    clear();
    // Scan 1 publishes the placeholder; the poller then reports it is polling.
    record_scan(vec![row("telegram/izzie", STARTING)], Vec::new());
    record_state("telegram/izzie", "polling", None);
    // Scan 2 finds the same bot and publishes the placeholder again.
    record_scan(vec![row("telegram/izzie", STARTING)], Vec::new());
    let snapshot = snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"));
    assert_eq!(
        snapshot.bots[0].state, "polling",
        "a rescan must not demote a live poller to `starting`: {:?}",
        snapshot.bots[0]
    );

    // A live detail survives with its state, and a bot the scan no longer finds
    // takes nothing forward.
    record_state(
        "telegram/izzie",
        "waiting-for-lock",
        Some("PID 4242 holds this bot's gateway lock".into()),
    );
    record_scan(vec![row("telegram/izzie", STARTING)], Vec::new());
    let snapshot = snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"));
    assert_eq!(snapshot.bots[0].state, "waiting-for-lock");
    assert!(
        snapshot.bots[0]
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("4242")),
        "the live reason must travel with the state it explains: {:?}",
        snapshot.bots[0]
    );
    record_scan(vec![row("telegram/cto", STARTING)], Vec::new());
    let snapshot = snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"));
    assert_eq!(
        snapshot.bots[0].state, STARTING,
        "a bot the previous scan never saw inherits nothing: {:?}",
        snapshot.bots[0]
    );
    clear();
}

/// The separate `tagent system status` process holds no gateway state, so the
/// live lock probe is the only thing that can report a running poller.
#[test]
#[serial_test::serial]
fn telegram_gateway_status_snapshot_reports_a_live_lock_holder() {
    clear();
    let dir = tempfile::tempdir().expect("tempdir");
    // Held for the whole assertion — closing the file releases the lock.
    let _guard = crate::telegram::acquire_gateway_lock_for_test(gateway_lock_path(dir.path()))
        .expect("a free lock must be acquirable");

    let held = snapshot_at(dir.path());
    assert_eq!(
        held.lock_holders,
        vec![std::process::id() as i32],
        "a held lock names its holder"
    );

    drop(_guard);
    // #9220: release is eventual — a sibling test's child can still hold a
    // copy of the descriptor between `fork` and `exec`.
    let free = lock_holders_once_released(dir.path());
    assert!(
        free.is_empty(),
        "closing the descriptor releases the lock: {free:?}"
    );
    clear();
}

/// How long a released gateway lock may still read as held.
const RELEASE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// The fixture bot's gateway lock file inside `dir`.
fn gateway_lock_path(dir: &std::path::Path) -> std::path::PathBuf {
    let key = crate::telegram::BotKey::from_token(FIXTURE_TOKEN);
    dir.join(format!("telegram-{}.pid", key.digest()))
}

/// The lock holders at `dir` once they clear, or the last read at the deadline.
///
/// Why (#9220): every lib test is a thread of one process, so a sibling's
/// child holds a copy of the lock descriptor between `fork` and `exec`, and a
/// `flock` stays held until its last copy closes. Release right after the
/// guard drops is therefore eventual, not immediate.
/// What: re-reads `snapshot_at(dir).lock_holders` every 10 ms until it is
/// empty or [`RELEASE_DEADLINE`] passes, and returns the last read.
/// Test: `telegram_gateway_lock_release_waits_out_a_child_in_its_fork_exec_window`.
fn lock_holders_once_released(dir: &std::path::Path) -> Vec<i32> {
    let deadline = std::time::Instant::now() + RELEASE_DEADLINE;
    loop {
        let holders = snapshot_at(dir).lock_holders;
        if holders.is_empty() || std::time::Instant::now() >= deadline {
            return holders;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// #9220 regression: a child forked while the gateway lock is held keeps the
/// lock until it execs, and the release check waits that out.
///
/// Why: this is the CI interleaving made certain — the child is parked in its
/// fork-to-exec window when the guard drops.
/// What: the child's `pre_exec` hook signals on a pipe, then sleeps 300 ms; the
/// guard drops only after the signal arrives. An immediate release assertion
/// failed here every run.
/// Test: this test.
#[test]
#[serial_test::serial]
fn telegram_gateway_lock_release_waits_out_a_child_in_its_fork_exec_window() {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let guard = crate::telegram::acquire_gateway_lock_for_test(gateway_lock_path(dir.path()))
        .expect("a free lock must be acquirable");
    let (mut forked, signal) = std::io::pipe().expect("pipe");
    let spawner = std::thread::spawn(move || {
        let fd = signal.as_raw_fd();
        let mut child = std::process::Command::new("true");
        // SAFETY: the hook runs in the forked child before `exec` and calls
        // only `write` and `usleep`, both async-signal-safe.
        unsafe {
            child.pre_exec(move || {
                let _ = libc::write(fd, b"f".as_ptr().cast(), 1);
                let _ = libc::usleep(300_000);
                Ok(())
            });
        }
        let status = child.status();
        // An early spawn failure closes the pipe, so the read below fails
        // instead of hanging.
        drop(signal);
        status
    });
    let mut byte = [0_u8; 1];
    forked
        .read_exact(&mut byte)
        .expect("the child forked and holds a copy of the lock descriptor");

    drop(guard);
    let holders = lock_holders_once_released(dir.path());
    let status = spawner.join().expect("spawner thread");
    assert!(
        holders.is_empty(),
        "the lock outlived the child's exec: {holders:?}"
    );
    assert!(status.expect("child ran").success());
}

/// #9220 guard: the gateway lock descriptor does not survive `exec`.
///
/// Why: the polling in [`lock_holders_once_released`] tolerates a child in its
/// fork-to-exec window. A descriptor that leaked through `exec` would instead
/// hold the lock for the child's whole life — a real second-poller bug — and
/// this test stays red at any deadline.
/// What: holds the lock, spawns `sleep 30` (`spawn` returns after `exec`),
/// drops the guard, and requires release within [`RELEASE_DEADLINE`]. The
/// child is killed and reaped on every exit path, panics included.
/// Test: this test.
#[test]
#[serial_test::serial]
fn telegram_gateway_lock_does_not_survive_into_an_execed_child() {
    /// Kills and reaps the child when dropped, so no process outlives the test.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let guard = crate::telegram::acquire_gateway_lock_for_test(gateway_lock_path(dir.path()))
        .expect("a free lock must be acquirable");
    let _child = KillOnDrop(
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep"),
    );

    drop(guard);
    let holders = lock_holders_once_released(dir.path());
    assert!(
        holders.is_empty(),
        "the lock descriptor leaked into an exec'd child: {holders:?}"
    );
}

/// No bot token and no digest of one may appear anywhere an operator or an
/// LLM can read — not in a row, not in a warning, not in the rendered text.
#[test]
#[serial_test::serial]
fn telegram_gateway_status_never_renders_the_bot_token() {
    clear();
    let key = crate::telegram::BotKey::from_token(FIXTURE_TOKEN);
    let bot = crate::telegram::TelegramBot::new(
        trusty_common::credentials::Secret::new(FIXTURE_TOKEN.to_string()),
        Some(vec!["izzie".into()]),
        vec!["telegram/izzie".into()],
    );
    record_scan(
        vec![TelegramBotStatus {
            credential_refs: bot.label(),
            assistants: bot.owners().unwrap_or_default().to_vec(),
            state: "polling".into(),
            detail: None,
        }],
        Vec::new(),
    );
    let snapshot = snapshot_at(std::path::Path::new("/nonexistent-telegram-state-dir"));
    let rendered = format!(
        "{}\n{}\n{:?}",
        serde_json::to_string(&snapshot).expect("status serializes"),
        crate::system_status::format::render_text(&report_with(snapshot.clone())),
        bot
    );
    assert!(
        !rendered.contains(FIXTURE_TOKEN),
        "the bot token must never be rendered: {rendered}"
    );
    assert!(
        !rendered.contains(key.digest()),
        "nor its digest, which is a stable correlator for it: {rendered}"
    );
    assert!(
        rendered.contains("telegram/izzie"),
        "the operator's own credential reference IS the label: {rendered}"
    );
    clear();
}

/// A report carrying only what the gateway section renders.
fn report_with(
    telegram_gateway: TelegramGatewayStatus,
) -> crate::system_status::SystemStatusReport {
    crate::system_status::SystemStatusReport {
        tagent: crate::system_status::TagentSelfStatus {
            version: "9.9.9".into(),
            active_agent: "izzie".into(),
            model: "anthropic/claude-opus-4-6".into(),
            runner: "subprocess".into(),
        },
        daemons: Vec::new(),
        mcp_servers: Vec::new(),
        credentials: Vec::new(),
        stores: Vec::new(),
        unresolved_bindings: Vec::new(),
        telegram_gateway,
        agent_registry_count: 0,
        skills_count: 0,
    }
}
