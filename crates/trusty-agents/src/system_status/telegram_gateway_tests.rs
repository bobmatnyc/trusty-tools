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
#[serial_test::serial(telegram_gateway_status)]
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
#[serial_test::serial(telegram_gateway_status)]
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
#[serial_test::serial(telegram_gateway_status)]
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
#[serial_test::serial(telegram_gateway_status)]
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
#[serial_test::serial(telegram_gateway_status)]
fn telegram_gateway_status_snapshot_reports_a_live_lock_holder() {
    clear();
    let dir = tempfile::tempdir().expect("tempdir");
    let key = crate::telegram::BotKey::from_token(FIXTURE_TOKEN);
    let path = dir.path().join(format!("telegram-{}.pid", key.digest()));
    // Held for the whole assertion — closing the file releases the lock.
    let _guard = crate::telegram::acquire_gateway_lock_for_test(path.clone())
        .expect("a free lock must be acquirable");

    let held = snapshot_at(dir.path());
    assert_eq!(
        held.lock_holders,
        vec![std::process::id() as i32],
        "a held lock names its holder"
    );

    drop(_guard);
    let free = snapshot_at(dir.path());
    assert!(
        free.lock_holders.is_empty(),
        "closing the descriptor releases the lock: {:?}",
        free.lock_holders
    );
    clear();
}

/// No bot token and no digest of one may appear anywhere an operator or an
/// LLM can read — not in a row, not in a warning, not in the rendered text.
#[test]
#[serial_test::serial(telegram_gateway_status)]
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
