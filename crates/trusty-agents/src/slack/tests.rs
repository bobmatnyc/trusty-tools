//! Unit tests for the Slack gateway's pure helpers.
//!
//! Why: Message formatting, the pairing state machine, envelope dedup, and the
//! RBAC parser are all unit-testable without a live Slack workspace. This
//! module covers them; live verification needs real app/bot tokens.
//! What: Tests for `split_message`, `markdown_to_mrkdwn`, `verify_pair_attempt`,
//! `dedup_check_and_record`, and the RBAC env/allow-list parsing.
//! Test: This module is itself the test coverage.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::format::{
    MAX_SLACK_MESSAGE, convert_double_to_single_asterisk, markdown_to_mrkdwn, split_message,
};
use super::pairing::{
    PAIRING_CODE_TTL, PairOutcome, SENTINEL_PAIRING_CHANNEL_ID, generate_pairing_code,
    issue_repl_pairing_code, new_pending_pairs, verify_pair_attempt,
};
use super::rbac::{SlackRbacConfig, VIRTUAL_CTO_MESSAGE, default_rbac_users, parse_rbac_users};
use super::{ENVELOPE_DEDUP_CAP, dedup_check_and_record};

#[test]
fn split_message_short() {
    let chunks = split_message("hello", MAX_SLACK_MESSAGE);
    assert_eq!(chunks, vec!["hello".to_string()]);
}

#[test]
fn split_message_newline_boundary() {
    let line = "a".repeat(100);
    let text = format!("{}\n{}", line, line);
    let chunks = split_message(&text, 150);
    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].ends_with('\n'));
    assert_eq!(chunks[1], line);
}

#[test]
fn split_message_hard_split_no_newline() {
    let text = "a".repeat(200);
    let chunks = split_message(&text, 100);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 100);
    assert_eq!(chunks[1].len(), 100);
}

#[test]
fn split_message_utf8_safe() {
    // 4-byte chars at the boundary must not be split mid-sequence.
    let text = "🦀".repeat(50); // 200 bytes
    let chunks = split_message(&text, 99);
    let joined: String = chunks.join("");
    assert_eq!(joined, text, "round-trip must match");
}

#[test]
fn markdown_to_mrkdwn_bold_conversion() {
    let out = markdown_to_mrkdwn("this is **important**!");
    assert_eq!(out, "this is *important*!");
}

#[test]
fn markdown_to_mrkdwn_preserves_code_fences() {
    let input = "before\n```rust\nlet x = 1;\n```\nafter";
    let out = markdown_to_mrkdwn(input);
    // Slack mrkdwn accepts ``` fences natively — leave as-is.
    assert!(out.contains("```"), "got: {}", out);
    assert!(out.contains("let x = 1;"), "got: {}", out);
}

#[test]
fn markdown_to_mrkdwn_preserves_inline_code() {
    let out = markdown_to_mrkdwn("call `foo()` then");
    assert!(out.contains("`foo()`"), "got: {}", out);
}

#[test]
fn convert_double_to_single_asterisk_alternates() {
    let out = convert_double_to_single_asterisk("a **b** c **d** e");
    assert_eq!(out, "a *b* c *d* e");
}

#[test]
fn convert_double_to_single_asterisk_unbalanced_passes_through() {
    let out = convert_double_to_single_asterisk("a **b c");
    assert_eq!(out, "a **b c");
}

#[test]
fn pairing_code_is_six_digits() {
    for _ in 0..100 {
        let code = generate_pairing_code();
        assert_eq!(code.len(), 6, "code {code} not 6 chars");
        assert!(
            code.chars().all(|c| c.is_ascii_digit()),
            "code {code} not all digits"
        );
    }
}

#[test]
fn pair_no_pending_returns_no_pending() {
    let outcome = verify_pair_attempt(None, "123456", Instant::now(), PAIRING_CODE_TTL);
    assert_eq!(outcome, PairOutcome::NoPending);
}

#[test]
fn pair_expired_code_is_rejected() {
    let issued = Instant::now();
    let now = issued + PAIRING_CODE_TTL + Duration::from_secs(1);
    let entry = ("123456".to_string(), issued);
    let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
    assert_eq!(outcome, PairOutcome::Expired);
}

#[test]
fn pair_mismatch_is_rejected() {
    let issued = Instant::now();
    let entry = ("123456".to_string(), issued);
    let outcome = verify_pair_attempt(Some(&entry), "654321", issued, PAIRING_CODE_TTL);
    assert_eq!(outcome, PairOutcome::Mismatch);
}

#[test]
fn pair_valid_code_succeeds() {
    let issued = Instant::now();
    let entry = ("123456".to_string(), issued);
    let now = issued + Duration::from_secs(60);
    let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
    assert_eq!(outcome, PairOutcome::Success);
}

/// REPL-issued code lands under the sentinel key.
#[tokio::test]
async fn repl_issued_code_lands_under_sentinel() {
    let pending = new_pending_pairs();
    let code = issue_repl_pairing_code(&pending).await;
    assert_eq!(code.len(), 6);
    let map = pending.lock().await;
    let entry = map
        .get(&SENTINEL_PAIRING_CHANNEL_ID)
        .expect("sentinel entry");
    assert_eq!(entry.0, code);
}

/// A `/slack-pair <code>` from any channel can claim the sentinel entry.
#[tokio::test]
async fn repl_issued_code_promotes_channel_via_sentinel() {
    let pending = new_pending_pairs();
    let code = issue_repl_pairing_code(&pending).await;
    let now = Instant::now();
    let map = pending.lock().await;
    let outcome = verify_pair_attempt(
        map.get(&SENTINEL_PAIRING_CHANNEL_ID),
        &code,
        now,
        PAIRING_CODE_TTL,
    );
    assert_eq!(outcome, PairOutcome::Success);
}

/// Sentinel entry past TTL returns Expired.
#[test]
fn sentinel_expired_code_is_rejected() {
    let issued = Instant::now();
    let entry = ("123456".to_string(), issued);
    let now = issued + PAIRING_CODE_TTL + Duration::from_secs(1);
    let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
    assert_eq!(outcome, PairOutcome::Expired);
}

/// With nothing under the sentinel, lookup returns NoPending.
#[tokio::test]
async fn empty_pending_map_returns_no_pending() {
    let pending = new_pending_pairs();
    let map = pending.lock().await;
    let outcome = verify_pair_attempt(
        map.get(&SENTINEL_PAIRING_CHANNEL_ID),
        "123456",
        Instant::now(),
        PAIRING_CODE_TTL,
    );
    assert_eq!(outcome, PairOutcome::NoPending);
}

#[tokio::test]
async fn dedup_skips_duplicate_envelopes() {
    let dedup: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(ENVELOPE_DEDUP_CAP)));
    assert!(dedup_check_and_record(&dedup, "env_1").await);
    assert!(!dedup_check_and_record(&dedup, "env_1").await);
    assert!(dedup_check_and_record(&dedup, "env_2").await);
}

/// An unknown Slack user (absent from the RBAC table) must get the static
/// Virtual-CTO reply and never reach the LLM (#481).
#[test]
fn rbac_unknown_user_returns_virtual_cto_message() {
    let cfg = SlackRbacConfig {
        users: default_rbac_users(),
        default_persona: "cto-assistant".to_string(),
    };
    // A user id that is not in the default team table.
    assert!(cfg.user("U_UNKNOWN_999").is_none());
    // The handler returns `VIRTUAL_CTO_MESSAGE` verbatim for this case;
    // assert the constant carries the expected gating language.
    assert!(VIRTUAL_CTO_MESSAGE.starts_with(":lock:"));
    assert!(VIRTUAL_CTO_MESSAGE.contains("Duetto engineering team"));
    assert!(VIRTUAL_CTO_MESSAGE.contains("don't have access to"));
}

/// `SlackRbacConfig::from_env` parses a hardcoded `SLACK_RBAC_USERS`
/// string into the expected user table (#481).
#[test]
fn rbac_config_parses_env_string() {
    let users = parse_rbac_users(
        "U0A6V2W1M2R:Masa:ALL:*,\
         U0ALDQLBU79:Andrea:ALL:cto-assistant,\
         U09331EP3MX:Alex:ANALYTICS:cto-assistant+ctrl",
    );
    assert_eq!(users.len(), 3);

    let masa = users.get("U0A6V2W1M2R").expect("masa entry");
    assert_eq!(masa.name, "Masa");
    assert_eq!(masa.tier, crate::rbac::ServiceTier::All);
    assert!(masa.allowed_personas.is_none(), "`*` => unrestricted");

    let andrea = users.get("U0ALDQLBU79").expect("andrea entry");
    assert_eq!(andrea.tier, crate::rbac::ServiceTier::All);
    assert_eq!(
        andrea.allowed_personas.as_deref(),
        Some(&["cto-assistant".to_string()][..])
    );

    let alex = users.get("U09331EP3MX").expect("alex entry");
    assert_eq!(alex.tier, crate::rbac::ServiceTier::Analytics);
    assert_eq!(
        alex.allowed_personas.as_deref(),
        Some(&["cto-assistant".to_string(), "ctrl".to_string()][..])
    );

    // Malformed / unknown-tier entries are skipped, not fatal.
    let partial = parse_rbac_users("BAD:entry,U1:Name:NOPE:*,U2:Ok:ALL:*");
    assert_eq!(partial.len(), 1);
    assert!(partial.contains_key("U2"));
}

/// A restricted (non-`*`) user must be blocked from `/slack-switch`-ing to
/// a persona outside their allow-list (#481).
#[test]
fn switch_command_blocked_for_restricted_persona() {
    let users = default_rbac_users();
    // Andrea is `ALL:cto-assistant` — only `cto-assistant` is allowed.
    let andrea = users.get("U0ALDQLBU79").expect("andrea entry");
    let allowed = andrea
        .allowed_personas
        .as_ref()
        .expect("andrea has a restricted allow-list");
    // `ctrl` is NOT in the allow-list → switch must be rejected.
    assert!(!allowed.iter().any(|p| p == "ctrl"));
    // `cto-assistant` IS in the allow-list → switch would be permitted.
    assert!(allowed.iter().any(|p| p == "cto-assistant"));

    // Masa is `ALL:*` — unrestricted, may switch to anything incl. `ctrl`.
    let masa = users.get("U0A6V2W1M2R").expect("masa entry");
    assert!(
        masa.allowed_personas.is_none(),
        "unrestricted user may switch to any persona"
    );
}

/// Kartik Yellepeddi (U09J5EFTSA3) must be present in the default RBAC table
/// with `ALL` tier and a `cto-assistant`-restricted persona allow-list —
/// parity with the retiring custom `cto_bot`'s `BOT_ALLOWED_USERS` default
/// table (#3852).
#[test]
fn default_rbac_users_includes_kartik_parity() {
    let users = default_rbac_users();
    let kartik = users.get("U09J5EFTSA3").expect("kartik entry present");
    assert_eq!(kartik.name, "Kartik");
    assert_eq!(kartik.tier, crate::rbac::ServiceTier::All);
    assert_eq!(
        kartik.allowed_personas.as_deref(),
        Some(&["cto-assistant".to_string()][..])
    );
}

// ---------------------------------------------------------------------------
// #3852 hybrid architecture: `handlers::record_listener_event` mirrors
// inbound Slack messages onto the harness eventstream. These tests exercise
// it directly against a temp `$HOME` (same `HOME_LOCK` pattern as
// `listeners::store::tests` / `listeners::poll::tests`), asserting the
// SAME append-then-filter contract `listeners::poll::poll_once` pins for
// Gmail: the event is always durably appended, and the `included` flag
// mirrored onto `Event::ListenerEventReceived` reflects the CURRENT
// `filters.json` state at publish time, not a stale snapshot.
// ---------------------------------------------------------------------------
#[allow(clippy::await_holding_lock)]
mod eventstream_tests {
    use crate::listeners::store::EventStore;
    use crate::slack::handlers::record_listener_event;
    use crate::test_env::HOME_LOCK;

    fn set_test_home(dir: &std::path::Path) {
        // SAFETY: caller holds `HOME_LOCK` for the duration of the test (see
        // `crate::test_env`'s module doc) so no other thread observes `HOME`
        // mid-mutation.
        unsafe {
            std::env::set_var("HOME", dir);
        }
    }

    /// A message reaching `record_listener_event` is durably appended to
    /// the store with the expected `slack`/`message.<channel_type>` shape,
    /// and — with no filter ever set — is reported `included` (default
    /// `true`, matching `EventStore::is_event_type_included`'s documented
    /// default).
    #[tokio::test]
    async fn slack_listener_event_appends_and_respects_filter() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());

        record_listener_event("C123", "170000.001", "im", "Masa", "hello world").await;

        let events = EventStore::read_events(None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "slack:C123:170000.001");
        assert_eq!(events[0].listener_id, "slack");
        assert_eq!(events[0].provider, "slack");
        assert_eq!(events[0].event_type, "message.im");
        assert_eq!(events[0].from.as_deref(), Some("Masa"));
        assert!(events[0].included, "no filter set yet ⇒ default included");
    }

    /// Once an operator excludes `message.im` via `EventStore::set_filter`
    /// (the same mechanism `POST /api/listener-events/filter` uses), a
    /// SUBSEQUENT Slack message of that type is still durably appended
    /// (never silently dropped) but is reported `included: false` — proving
    /// `record_listener_event` consults the filter AFTER appending, exactly
    /// like `listeners::poll::poll_once` does for Gmail, rather than gating
    /// the append itself on the filter.
    #[tokio::test]
    async fn slack_listener_event_excluded_type_still_appended() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());

        EventStore::set_filter("message.im", false).await.unwrap();
        record_listener_event("C456", "170000.002", "im", "Andrea", "second message").await;

        let events = EventStore::read_events(None).await.unwrap();
        assert_eq!(
            events.len(),
            1,
            "excluded types are still appended, not dropped"
        );
        assert_eq!(events[0].id, "slack:C456:170000.002");
        assert!(!events[0].included, "excluded filter must be reflected");

        // A different, never-toggled event type on the same store stays
        // included — the filter is keyed per event_type, not global.
        record_listener_event("C456", "170000.003", "mpim", "Andrea", "group message").await;
        let events = EventStore::read_events(None).await.unwrap();
        let mpim_event = events
            .iter()
            .find(|e| e.event_type == "message.mpim")
            .expect("mpim event present");
        assert!(mpim_event.included, "message.mpim was never excluded");
    }
}
