//! Unit tests for the Telegram drive surface.
//!
//! Why: this lives in a sibling `tests.rs` (a recognized test-file path) so the
//! production `telegram/mod.rs` stays under the 500-SLOC production cap while the
//! suite shares `mod.rs`'s private items via `use super::*`.
//! What: covers token/username resolution, authorization, the action-capable
//! coordinator reply path, the bounded chat-history recorder, the action footer,
//! and command dispatch against a real in-process daemon router.
//! Test: this *is* the test module.

use super::*;

#[test]
fn resolve_token_reads_dotenv() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    let mut file = std::fs::File::create(&path).unwrap();
    writeln!(file, "TELEGRAM_BOT_TOKEN=\"123:ABC\"").unwrap();
    let value = read_dotenv_key(&path, "TELEGRAM_BOT_TOKEN");
    assert_eq!(value.as_deref(), Some("123:ABC"));
}

#[test]
fn resolve_token_missing_is_none() {
    let value = read_dotenv_key(Path::new("/no/such/.env"), "TELEGRAM_BOT_TOKEN");
    assert!(value.is_none());
}

#[test]
fn resolve_username_uses_env_when_set() {
    // An explicit value wins over the default and is trimmed.
    assert_eq!(resolve_username(Some("my_bot".into())), "my_bot");
    assert_eq!(resolve_username(Some("  my_bot  ".into())), "my_bot");
}

#[test]
fn resolve_username_falls_back_when_unset() {
    // No env var → the real deployed bot default.
    assert_eq!(resolve_username(None), DEFAULT_BOT_USERNAME);
    assert_eq!(resolve_username(None), "t_sess_bot");
}

#[test]
fn resolve_username_falls_back_when_empty() {
    // Empty or whitespace-only env value is treated as unset.
    assert_eq!(resolve_username(Some(String::new())), DEFAULT_BOT_USERNAME);
    assert_eq!(resolve_username(Some("   ".into())), DEFAULT_BOT_USERNAME);
}

#[test]
fn authorization_respects_allowed_user() {
    let unrestricted = BotOptions::default();
    assert!(is_authorized(&unrestricted, Some(7)));
    assert!(is_authorized(&unrestricted, None));

    let restricted = BotOptions {
        allowed_user_id: Some(42),
        alert_chat_id: None,
    };
    assert!(is_authorized(&restricted, Some(42)));
    assert!(!is_authorized(&restricted, Some(99)));
    assert!(!is_authorized(&restricted, None));
}

/// Spawn the daemon's real HTTP API on a random loopback port.
///
/// Why: lets the bot's command dispatch be tested against the genuine
/// daemon routes without a live daemon, tmux, or external network.
/// What: builds `api::router(DaemonState::shared())`, binds an ephemeral
/// port, serves it on a background task, and returns the state plus base URL.
/// Test: used by the `dispatch_*` tests below.
async fn spawn_test_daemon() -> (std::sync::Arc<crate::daemon::state::DaemonState>, String) {
    use crate::daemon::{api, state::DaemonState};
    use std::future::IntoFuture;
    // Root the daemon's persisted state at a throwaway temp directory so
    // the test never reads (or writes) the operator's real pairing record.
    // `keep` leaks the directory so it outlives the background server.
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root(root));
    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    (state, format!("http://{addr}"))
}

/// Spawn a hermetic test daemon whose SM is enabled but wired to a degraded
/// resolver (no inference provider), forcing the no-creds explanatory path.
///
/// Why: `spawn_test_daemon` calls `DaemonState::with_root → DaemonState::new`
/// which reads the REAL `~/.trusty-mpm/config.toml`. On a developer machine
/// with `[session_manager] enabled = true` and `OPENROUTER_API_KEY` set, that
/// leaks live config into the test and routes the message to a real LLM call
/// instead of the Degraded path. This variant injects a controlled SM agent that
/// is always enabled but always reports Degraded (no inference credentials), so
/// the action-coordinator path deterministically returns the explanatory 200
/// reply regardless of the operator's local config or environment variables.
/// What: builds `DaemonState::with_session_manager_agent` carrying an
/// `enabled = true` agent backed by `MockResolver::degraded()`, serves the
/// daemon router on a loopback port, and returns the state plus base URL.
/// Test: used by `action_chat_reply_reports_unconfigured`.
async fn spawn_test_daemon_degraded_sm()
-> (std::sync::Arc<crate::daemon::state::DaemonState>, String) {
    use crate::core::sm::SessionManagerAgent;
    use crate::core::sm::agent::mock::MockResolver;
    use crate::core::sm::config::SessionManagerConfig;
    use crate::daemon::{api, state::DaemonState};
    use std::future::IntoFuture;

    let tmp = tempfile::tempdir().unwrap().keep();
    let enabled_cfg = SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    };
    let agent = std::sync::Arc::new(SessionManagerAgent::for_test(
        enabled_cfg,
        std::sync::Arc::new(MockResolver::degraded()),
        tmp,
    ));
    let state = std::sync::Arc::new(DaemonState::with_session_manager_agent(agent));
    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    (state, format!("http://{addr}"))
}

#[tokio::test]
async fn action_chat_reply_reports_unconfigured() {
    // When the SM is enabled but inference is not wired (no provider credentials),
    // the action-coordinator returns HTTP 200 with an explanatory reply (#1524:
    // graceful degradation replaces the previous opaque 503). The Telegram bot
    // relays that explanatory text to the operator rather than surfacing an HTTP
    // error — the new no-creds contract.
    //
    // Uses a hermetic daemon with a degraded SM resolver so the test is independent
    // of the operator's ~/.trusty-mpm/config.toml and OPENROUTER_API_KEY env var.
    let (_state, url) = spawn_test_daemon_degraded_sm().await;
    let executor = CommandExecutor::new(url);
    let histories: ChatHistories = Arc::new(Mutex::new(HashMap::new()));
    let reply = action_chat_reply(&executor, &histories, 42, "hello there").await;
    // The explanatory reply must tell the operator why inference is unavailable.
    assert!(
        reply.contains("inference"),
        "reply must mention 'inference', got: {reply:?}"
    );
    assert!(
        reply.contains("OPENROUTER_API_KEY")
            || reply.contains("configured")
            || reply.contains("credentials"),
        "reply must hint at configuring credentials, got: {reply:?}"
    );
    // The explanatory turn IS recorded so follow-up messages see coherent history
    // (coordinator returned Ok(Some(outcome)) with the notice as the reply text).
    assert!(
        histories.lock().unwrap().get(&42).is_some(),
        "explanatory turn must be stored in chat history"
    );
}

#[test]
fn record_chat_turn_caps_history() {
    // Pushing far past the cap must clamp the stored history to the last
    // MAX_CHAT_HISTORY_TURNS messages and drop the oldest first, so a
    // long-lived chat can't grow the history (and the re-sent prompt)
    // without bound.
    let mut entry: Vec<ChatMessage> = Vec::new();
    let turns = MAX_CHAT_HISTORY_TURNS * 2; // each turn pushes 2 messages
    for i in 0..turns {
        record_chat_turn(&mut entry, &format!("user {i}"), &format!("reply {i}"));
    }
    // Length is clamped to the cap, never above it.
    assert_eq!(entry.len(), MAX_CHAT_HISTORY_TURNS);
    // The oldest messages were dropped: the very first user turn is gone…
    assert!(
        !entry.iter().any(|m| m.content == "user 0"),
        "oldest turn must be evicted"
    );
    // …and the most recent turn is retained.
    let last = turns - 1;
    assert_eq!(entry.last().unwrap().content, format!("reply {last}"));
}

#[test]
fn record_chat_turn_skips_empty_reply() {
    // An empty assistant reply must not be stored — only the user turn is
    // persisted — so the context window isn't polluted with a blank message.
    let mut entry: Vec<ChatMessage> = Vec::new();
    record_chat_turn(&mut entry, "hello", "");
    assert_eq!(entry.len(), 1);
    assert_eq!(entry[0].role, "user");
    assert_eq!(entry[0].content, "hello");
    // A non-empty reply still stores both turns.
    record_chat_turn(&mut entry, "again", "there");
    assert_eq!(entry.len(), 3);
    assert_eq!(entry[2].role, "assistant");
    assert_eq!(entry[2].content, "there");
}

#[test]
fn action_footer_lists_verbs() {
    // A non-empty audit trail renders as a compact italic footer so the
    // operator sees exactly which verbs the free-text turn executed.
    let verbs = vec!["sessions.health".to_string(), "sessions.list".to_string()];
    let footer = action_footer(Some(&verbs)).expect("footer for non-empty verbs");
    assert_eq!(footer, "\n\n<i>ran: sessions.health, sessions.list</i>");
}

#[test]
fn action_footer_absent_when_empty() {
    // No verbs ran (None) or an empty list both suppress the footer — a
    // text-only turn must not advertise side effects.
    assert!(action_footer(None).is_none());
    assert!(action_footer(Some(&[])).is_none());
}

#[tokio::test]
async fn dispatch_help_returns_help() {
    let executor = CommandExecutor::new("http://unused");
    let result = dispatch_command(TelegramCommand::Help, &executor, 1).await;
    assert!(matches!(result, CommandResult::Help(_)));
}

#[tokio::test]
async fn dispatch_start_with_no_code_queries_state() {
    // `/start` with no code is a pairing-status query against the daemon.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    let result = dispatch_command(TelegramCommand::Start(String::new()), &executor, 1).await;
    match result {
        CommandResult::PairState { paired } => assert!(!paired),
        other => panic!("expected PairState, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_start_with_deep_link_code_confirms() {
    // `/start <code>` (the `?start=` deep-link form) confirms the code.
    let (state, url) = spawn_test_daemon().await;
    let code = state.generate_pair_code();
    let executor = CommandExecutor::new(url);
    let result = dispatch_command(TelegramCommand::Start(code), &executor, 555).await;
    match result {
        CommandResult::PairSuccess { chat_info } => assert!(chat_info.contains("555")),
        other => panic!("expected PairSuccess, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_pair_with_bad_code_errors() {
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    let result = dispatch_command(TelegramCommand::Pair("ZZZZZZ".into()), &executor, 1).await;
    assert!(matches!(result, CommandResult::Error(_)));
}
