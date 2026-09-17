//! Unit tests for the Telegram gateway's pure helpers.
//!
//! Why: The message-formatting, pairing state-machine, persistence, and
//! single-instance PID guard are all unit-testable without a live bot. This
//! module covers them; live verification is out of scope (the bot is wired
//! behind `--telegram`).
//! What: Tests for `split_message`, `markdown_to_html_safe` and friends, the
//! `verify_pair_attempt` state machine, paired-chats round-trip, and the
//! PID-guard acquire/stale/drop behavior.
//! Test: This module is itself the test coverage.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teloxide::types::ChatId;
use tokio::sync::RwLock;

use super::MAX_TELEGRAM_MESSAGE;
use super::format::{
    convert_pairs, convert_pairs_outside_tag, markdown_to_html_safe, split_message, strip_html_tags,
};
use super::pairing::{
    PAIRING_CODE_TTL, PairOutcome, PairedChats, SENTINEL_PAIRING_CHAT_ID, TelegramPidGuard,
    generate_pairing_code, issue_repl_pairing_code, load_paired_chats, new_pending_pairs,
    save_paired_chats, verify_pair_attempt,
};
// #8190: the per-bot lock and pairing paths, and the read-only lock probe the
// supervisor runs before every poll attempt.
use super::bot::{BotKey, TelegramBot};
use super::pairing::{
    gateway_lock_holder_at, live_gateway_lock_holders_in, paired_chats_state_path_for,
    telegram_pid_file_path_for,
};

#[test]
fn split_message_short() {
    let chunks = split_message("hello", MAX_TELEGRAM_MESSAGE);
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
fn markdown_to_html_safe_escapes_lt_gt() {
    let out = markdown_to_html_safe("a < b > c");
    assert!(out.contains("&lt;"));
    assert!(out.contains("&gt;"));
}

#[test]
fn markdown_to_html_safe_fence_to_pre() {
    let input = "before\n```rust\nlet x = 1;\n```\nafter";
    let out = markdown_to_html_safe(input);
    assert!(out.contains("<pre><code>"), "got: {}", out);
    assert!(out.contains("</code></pre>"), "got: {}", out);
}

#[test]
fn markdown_to_html_safe_inline_code() {
    let out = markdown_to_html_safe("call `foo()` then");
    assert!(out.contains("<code>foo()</code>"), "got: {}", out);
}

#[test]
fn markdown_to_html_safe_bold() {
    let out = markdown_to_html_safe("this is **important**!");
    assert!(out.contains("<b>important</b>"), "got: {}", out);
}

#[test]
fn convert_pairs_alternates_open_close() {
    let out = convert_pairs("a `b` c `d` e", "`", "<c>", "</c>");
    assert_eq!(out, "a <c>b</c> c <c>d</c> e");
}

#[test]
fn convert_pairs_unbalanced_passes_through() {
    let out = convert_pairs("a `b c", "`", "<c>", "</c>");
    assert_eq!(out, "a `b c");
}

#[test]
fn strip_html_tags_removes_tags() {
    assert_eq!(strip_html_tags("<b>hi</b> there"), "hi there");
}

/// #419: Plain-text fallback must unescape HTML entities.
///
/// Why: When `markdown_to_html_safe` escapes `<` to `&lt;`, the HTML send
/// path renders it correctly. But if the HTML send fails and we fall back
/// to plain text via `strip_html_tags`, the user used to see literal
/// `&lt;` characters. After the fix, entities are decoded so the user
/// sees the original symbol.
/// Test: Round-trip "a < b & c" through escape + strip.
#[test]
fn strip_html_tags_unescapes_entities() {
    let escaped = "a &lt; b &amp; c &gt; d &quot;e&quot; &#39;f&#39;";
    let plain = strip_html_tags(escaped);
    assert_eq!(plain, "a < b & c > d \"e\" 'f'");
}

/// #419: `&amp;` must decode last so encoded entities don't double-decode.
///
/// Why: A string containing the literal text `&lt;` (user wrote "&lt;",
/// not "<") would round-trip to `&amp;lt;`. The strip path must yield
/// `&lt;`, not `<`.
/// Test: Encode then strip; the literal entity must survive.
#[test]
fn strip_html_tags_does_not_double_decode() {
    // User content: literal "&lt;" → escaped to "&amp;lt;" by html::escape.
    let escaped = "raw &amp;lt; here";
    let plain = strip_html_tags(escaped);
    assert_eq!(plain, "raw &lt; here");
}

/// #419: Bold markers inside backticks must NOT become <b> tags.
///
/// Why: A reply like `` `let x = **value**;` `` should render the `**`
/// literally inside the code span. The pre-fix order ran bold first
/// and produced "<code>let x = <b>value</b>;</code>", which Telegram
/// renders as literal "<b>value</b>" in monospace.
/// Test: Convert and assert no <b> tags appear inside the code span.
#[test]
fn markdown_to_html_safe_bold_inside_code_is_literal() {
    let out = markdown_to_html_safe("call `x = **literal**` here");
    assert!(out.contains("<code>x = **literal**</code>"), "got: {out}");
    assert!(!out.contains("<b>"), "<b> should not appear: {out}");
}

/// #419: Bold OUTSIDE code spans still works.
///
/// Why: Reversing the conversion order must not regress the common case.
/// Test: `**emph** and `code`` → bold on emph, code on code.
#[test]
fn markdown_to_html_safe_bold_outside_code_still_works() {
    let out = markdown_to_html_safe("**emph** and `code`");
    assert!(out.contains("<b>emph</b>"), "got: {out}");
    assert!(out.contains("<code>code</code>"), "got: {out}");
}

/// #419: convert_pairs_outside_tag skips inside <code> spans.
///
/// Why: Direct unit test of the helper that powers the bold-after-code
/// fix. Inside `<code>…</code>`, `**x**` must be left untouched.
/// Test: Manually wrap a code span and verify bold conversion only
/// touches the outside.
#[test]
fn convert_pairs_outside_tag_skips_code() {
    let input = "**a** <code>**b**</code> **c**";
    let out = convert_pairs_outside_tag(input, "**", "<B>", "</B>", "<code>", "</code>");
    assert_eq!(out, "<B>a</B> <code>**b**</code> <B>c</B>");
}

/// #419: convert_pairs_outside_tag handles unclosed code span defensively.
///
/// Why: If `markdown_to_html_safe` ever emits an unclosed `<code>` (it
/// shouldn't, but defense in depth matters), we must not loop or panic.
/// Test: Input with `<code>` and no `</code>` returns the prefix
/// converted plus the unclosed tail verbatim.
#[test]
fn convert_pairs_outside_tag_unclosed_does_not_panic() {
    let input = "**a** <code>tail";
    let out = convert_pairs_outside_tag(input, "**", "<B>", "</B>", "<code>", "</code>");
    assert_eq!(out, "<B>a</B> <code>tail");
}

/// #419: Empty input to split_message returns one empty chunk… or none?
///
/// Why: The dispatch path can in principle hand `send_long_html` an empty
/// string (e.g. an LLM that returns "" after error recovery). We must
/// not panic, and we must not try to send a zero-length Telegram message
/// (which would 400). Verify the split function returns a single
/// empty-string chunk for empty input — the caller's iteration then
/// hits `send_message(chat, "")` which Telegram itself rejects gracefully
/// via the existing error fallback.
/// Test: Empty in, single empty out.
#[test]
fn split_message_empty_input() {
    let chunks = split_message("", MAX_TELEGRAM_MESSAGE);
    assert_eq!(chunks, vec!["".to_string()]);
}

/// #419: split_message at exact boundary length stays one chunk.
///
/// Why: Off-by-one in the `text.len() <= max_len` check would split
/// strings that are exactly at the limit into two pieces, wasting a
/// round-trip. Verify equality with max_len is one chunk.
/// Test: 100-char string with max_len=100 → 1 chunk.
#[test]
fn split_message_exact_boundary() {
    let text = "a".repeat(100);
    let chunks = split_message(&text, 100);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len(), 100);
}

/// #419: Long fenced code block + bold around it survives conversion.
///
/// Why: End-to-end check that fence + bold + escaping compose cleanly.
/// This is the realistic shape of an LLM reply ("Here is the **fix**:
/// ```rust\nfn x() {}\n```").
/// Test: Verify the bold conversion happened, the fence is now <pre>,
/// and the angle brackets inside the code are escaped.
#[test]
fn markdown_to_html_safe_realistic_reply() {
    let input = "Here is the **fix**:\n```rust\nfn x<T>() {}\n```\nDone.";
    let out = markdown_to_html_safe(input);
    assert!(out.contains("<b>fix</b>"), "got: {out}");
    assert!(out.contains("<pre><code>"), "got: {out}");
    assert!(
        out.contains("fn x&lt;T&gt;()"),
        "angle brackets must be escaped: {out}"
    );
    assert!(out.contains("</code></pre>"), "got: {out}");
}

#[test]
fn pairing_code_is_six_digits() {
    // Why: Loop a few times to catch the zero-padding edge case where
    // rand happens to return a small number (e.g. 42 -> "000042").
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
    // Simulate "now" being TTL + 1s after issuance.
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
    // Within TTL.
    let now = issued + Duration::from_secs(60);
    let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
    assert_eq!(outcome, PairOutcome::Success);
}

/// #334: REPL-issued code lands under the sentinel key.
///
/// Why: The new flow has the REPL (not Telegram) generate the code and
/// store it under `SENTINEL_PAIRING_CHAT_ID`. Verifies that
/// `issue_repl_pairing_code` populates the map at the sentinel key.
/// Test: Call `issue_repl_pairing_code`, then assert the map has the
/// returned code under `SENTINEL_PAIRING_CHAT_ID`.
#[tokio::test]
async fn repl_issued_code_lands_under_sentinel() {
    let pending = new_pending_pairs();
    let code = issue_repl_pairing_code(&pending).await;
    assert_eq!(code.len(), 6);
    let map = pending.lock().await;
    let entry = map.get(&SENTINEL_PAIRING_CHAT_ID).expect("sentinel entry");
    assert_eq!(entry.0, code);
}

/// #334: A `/pair <code>` from any chat can claim the sentinel entry.
///
/// Why: This is the core security guarantee — the REPL issues the code,
/// any Telegram chat can validate against it. We verify the
/// `verify_pair_attempt` lookup against the sentinel returns Success.
/// Test: Issue code, then verify the same code against the sentinel entry.
#[tokio::test]
async fn repl_issued_code_promotes_chat_via_sentinel() {
    let pending = new_pending_pairs();
    let code = issue_repl_pairing_code(&pending).await;

    let now = Instant::now();
    let map = pending.lock().await;
    let outcome = verify_pair_attempt(
        map.get(&SENTINEL_PAIRING_CHAT_ID),
        &code,
        now,
        PAIRING_CODE_TTL,
    );
    assert_eq!(outcome, PairOutcome::Success);
}

/// #334: Sentinel entry past TTL returns Expired.
///
/// Why: TTL handling for sentinel entries must match per-chat entries.
/// Test: Build a synthetic entry with `issued` in the past and assert
/// `Expired`.
#[test]
fn sentinel_expired_code_is_rejected() {
    let issued = Instant::now();
    let entry = ("123456".to_string(), issued);
    let now = issued + PAIRING_CODE_TTL + Duration::from_secs(1);
    let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
    assert_eq!(outcome, PairOutcome::Expired);
}

/// #334: With nothing under the sentinel, lookup returns NoPending.
///
/// Why: A `/pair` arriving before the REPL has issued any code must be
/// rejected with NoPending so the user is told to run /telegram pair.
/// Test: Empty map -> sentinel lookup -> NoPending.
#[tokio::test]
async fn empty_pending_map_returns_no_pending() {
    let pending = new_pending_pairs();
    let map = pending.lock().await;
    let outcome = verify_pair_attempt(
        map.get(&SENTINEL_PAIRING_CHAT_ID),
        "123456",
        Instant::now(),
        PAIRING_CODE_TTL,
    );
    assert_eq!(outcome, PairOutcome::NoPending);
}

/// #467: Round-trip a `PairedChats` map through disk to verify
/// `save_paired_chats` + `load_paired_chats` preserve chat ids.
///
/// Why: Regression guard for the pairing-persistence feature. Without
/// this, a serializer or path-handling regression would silently break
/// every user's pairing on the next upgrade.
/// What: Insert two chats, save, load into a fresh map, verify both
/// chat ids survived.
/// Test: This is the test.
#[tokio::test]
async fn paired_state_round_trip() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram-paired.json");
    let paired: PairedChats = Arc::new(RwLock::new(HashMap::new()));
    {
        let mut g = paired.write().await;
        g.insert(ChatId(111), Instant::now());
        g.insert(ChatId(222), Instant::now());
    }
    save_paired_chats(&paired, &path)
        .await
        .expect("save should succeed");
    let loaded = load_paired_chats(&path).await;
    let g = loaded.read().await;
    assert!(g.contains_key(&ChatId(111)));
    assert!(g.contains_key(&ChatId(222)));
    assert_eq!(g.len(), 2);
}

/// #467: Missing state file is treated as "first run", not an error.
#[tokio::test]
async fn paired_state_missing_file_is_empty() {
    let tmp = tempdir_for_test();
    let path = tmp.join("does-not-exist.json");
    let loaded = load_paired_chats(&path).await;
    assert!(loaded.read().await.is_empty());
}

/// #467: A malformed JSON file must not panic; we fail open with empty.
#[tokio::test]
async fn paired_state_malformed_file_is_empty() {
    let tmp = tempdir_for_test();
    let path = tmp.join("broken.json");
    tokio::fs::write(&path, b"{not json").await.unwrap();
    let loaded = load_paired_chats(&path).await;
    assert!(loaded.read().await.is_empty());
}

/// #8190: `acquire` writes its PID for reporting and RELEASES the lock when
/// the guard drops — without unlinking the file, which is a lock, not a
/// liveness record.
#[test]
fn telegram_pid_guard_acquire_writes_and_releases() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram-abc.pid");
    {
        let _guard = TelegramPidGuard::acquire(path.clone()).expect("acquire");
        let contents = std::fs::read_to_string(&path).expect("lock file exists");
        assert_eq!(contents.trim(), std::process::id().to_string());
        assert_eq!(
            gateway_lock_holder_at(&path).and_then(|h| h.pid),
            Some(std::process::id() as i32),
            "a held lock names its holder"
        );
    }
    assert!(path.exists(), "the lock file is not unlinked on drop");
    assert!(
        gateway_lock_holder_at(&path).is_none(),
        "closing the descriptor releases the lock"
    );
}

/// #8190 code-critic MEDIUM 2: the pre-#8190 guard was check-then-act, so two
/// hosts could both read "stale" and both acquire. `flock` conflicts across two
/// descriptors even inside ONE process, which is what makes this testable.
#[test]
fn telegram_pid_guard_live_conflict_is_rejected() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram-abc.pid");
    let winner = TelegramPidGuard::acquire(path.clone()).expect("first acquire wins");
    let loser = TelegramPidGuard::acquire(path.clone());
    assert!(loser.is_err(), "a second acquirer must be refused");
    let message = format!("{:#}", loser.err().expect("an error"));
    assert!(
        message.contains(&std::process::id().to_string()),
        "the refusal must name the holder: {message}"
    );
    drop(winner);
    assert!(
        TelegramPidGuard::acquire(path).is_ok(),
        "the lock frees the moment the winner's descriptor closes"
    );
}

/// #8190: a PID file left behind by a crashed process is NOT a holder — the
/// kernel released the lock when the descriptor closed, so the next start takes
/// it immediately rather than waiting forever on a reused PID.
#[test]
fn telegram_pid_guard_reclaims_a_lock_no_one_holds() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram-abc.pid");
    // Our own (very much alive) PID, which the old `kill(pid, 0)` probe would
    // have read as a live holder forever.
    std::fs::write(&path, std::process::id().to_string()).unwrap();
    let _guard = TelegramPidGuard::acquire(path.clone()).expect("an unheld lock is reclaimable");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().trim(),
        std::process::id().to_string()
    );
}

/// #8190: the probe names a live holder, so the supervisor waits for it instead
/// of starting a second `getUpdates` that would terminate the first.
#[test]
fn telegram_gateway_lock_holder_reports_a_live_holder() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram-abc.pid");
    let _guard = TelegramPidGuard::acquire(path.clone()).expect("acquire");
    let holder = gateway_lock_holder_at(&path).expect("a live holder");
    assert_eq!(holder.pid, Some(std::process::id() as i32));
    assert_eq!(holder.label(), std::process::id().to_string());
}

/// #8190: an absent file, and a file nobody holds, are both free — the API host
/// must start after an unclean shutdown, not stay off forever.
#[test]
fn telegram_gateway_lock_holder_ignores_an_unlocked_file() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram-abc.pid");
    assert!(gateway_lock_holder_at(&path).is_none(), "absent file");
    std::fs::write(&path, i32::MAX.to_string()).unwrap();
    assert!(gateway_lock_holder_at(&path).is_none(), "unheld lock");
    std::fs::write(&path, "not-a-pid").unwrap();
    assert!(
        gateway_lock_holder_at(&path).is_none(),
        "unparseable, unheld"
    );
}

/// #8190: a locked file whose PID was not written yet is STILL a holder — the
/// lock is the fact, and the PID is only reporting text.
#[test]
fn telegram_gateway_lock_holder_reports_an_unnamed_holder() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram-abc.pid");
    let _guard = TelegramPidGuard::acquire(path.clone()).expect("acquire");
    std::fs::write(&path, "").unwrap();
    let holder = gateway_lock_holder_at(&path).expect("still held");
    assert_eq!(holder.pid, None);
    assert_eq!(holder.label(), "unknown");
}

/// #8190: `tagent system status` runs in another process, so the state-dir
/// sweep is what lets it report that something here is polling.
#[test]
fn telegram_gateway_live_lock_holders_sweep_the_state_dir() {
    let tmp = tempdir_for_test();
    let held = tmp.join("telegram-aaaa.pid");
    let free = tmp.join("telegram-bbbb.pid");
    let unrelated = tmp.join("telegram-paired-aaaa.json");
    std::fs::write(&free, "12345").unwrap();
    std::fs::write(&unrelated, "{}").unwrap();
    let _guard = TelegramPidGuard::acquire(held).expect("acquire");
    assert_eq!(
        live_gateway_lock_holders_in(&tmp),
        vec![std::process::id() as i32],
        "only a genuinely held lock counts"
    );
}

/// Owner ruling 2026-09-16: a chat paired to izzie's bot is not paired on
/// cto-assistant's — the pairing file is per bot.
#[tokio::test]
async fn telegram_pairing_state_is_per_bot() {
    let izzie = BotKey::from_token("111:izzie-token");
    let cto = BotKey::from_token("222:cto-token");
    assert_ne!(
        paired_chats_state_path_for(&izzie),
        paired_chats_state_path_for(&cto),
        "two bots must not share one pairing file"
    );
    assert_ne!(
        telegram_pid_file_path_for(&izzie),
        telegram_pid_file_path_for(&cto),
        "two bots must not share one gateway lock"
    );

    let tmp = tempdir_for_test();
    let izzie_path = tmp.join(format!("telegram-paired-{}.json", izzie.digest()));
    let cto_path = tmp.join(format!("telegram-paired-{}.json", cto.digest()));
    let paired: PairedChats = Arc::new(RwLock::new(HashMap::new()));
    paired.write().await.insert(ChatId(4242), Instant::now());
    save_paired_chats(&paired, &izzie_path).await.unwrap();

    assert_eq!(load_paired_chats(&izzie_path).await.read().await.len(), 1);
    assert!(
        load_paired_chats(&cto_path).await.read().await.is_empty(),
        "a pairing on izzie's bot must not authorize cto-assistant's"
    );
}

/// #8190: the key is a stable function of the token, which is what collapses
/// two bindings sharing one token onto one poller.
#[test]
fn telegram_bot_key_is_stable_per_token() {
    assert_eq!(
        BotKey::from_token("111:same").digest(),
        BotKey::from_token("111:same").digest()
    );
    assert_ne!(
        BotKey::from_token("111:a").digest(),
        BotKey::from_token("222:b").digest()
    );
}

/// #8190: no token and no digest of one may reach a log line. The key names a
/// file; the operator's credential reference names the bot everywhere else.
#[test]
fn telegram_bot_key_never_renders_the_token() {
    let token = "7427:AAH-fixture-bot-token";
    let key = BotKey::from_token(token);
    let bot = TelegramBot::new(
        trusty_common::credentials::Secret::new(token.to_string()),
        Some(vec!["izzie".into()]),
        vec!["telegram/izzie".into()],
    );
    let rendered = format!("{key:?} {bot:?} {}", bot.label());
    assert!(!rendered.contains(token), "token leaked: {rendered}");
    assert!(
        !rendered.contains(key.digest()),
        "digest leaked: {rendered}"
    );
    assert!(rendered.contains("telegram/izzie"), "{rendered}");
}

/// #8190: the digest may name a FILE, but the token itself never may.
#[test]
fn telegram_state_file_names_never_contain_the_token() {
    let token = "7427:AAH-fixture-bot-token";
    let key = BotKey::from_token(token);
    for path in [
        telegram_pid_file_path_for(&key),
        paired_chats_state_path_for(&key),
    ] {
        let rendered = path.display().to_string();
        assert!(!rendered.contains(token), "token in a path: {rendered}");
        assert!(rendered.contains(key.digest()), "keyed per bot: {rendered}");
    }
}

/// The VALUE half of the owner ruling: a scanned bot carries exactly the
/// assistants that own a binding on it, the legacy host-wide gateway carries
/// `None`, and neither renders anything key-shaped.
///
/// #8190 round-2 finding 3: this used to be named for the dispatch and assert
/// only the accessor, which states nothing about who wakes. The dispatch
/// narrowing itself is
/// `a_telegram_bots_owners_are_the_only_assistants_it_wakes`, which drives the
/// live loader and an injected dispatcher against two assistants.
#[test]
fn telegram_bot_carries_its_owners_and_never_renders_its_key() {
    let izzie = TelegramBot::new(
        trusty_common::credentials::Secret::new("111:izzie".to_string()),
        Some(vec!["izzie".into()]),
        vec!["telegram/izzie".into()],
    );
    assert_eq!(izzie.owners(), Some(["izzie".to_string()].as_slice()));
    assert_eq!(
        format!("{izzie:?}"),
        "TelegramBot { credential_refs: [\"telegram/izzie\"], owners: Some([\"izzie\"]), .. }",
        "the label is the whole rendered surface"
    );
    // The pre-#8190 standalone gateway keeps its any-assistant dispatch.
    let legacy = TelegramBot::new(
        trusty_common::credentials::Secret::new("111:izzie".to_string()),
        None,
        Vec::new(),
    );
    assert!(legacy.owners().is_none());
    assert_eq!(legacy.label(), "telegram (default credential)");
}

/// Creates a unique tempdir under the system temp, for the pid-guard tests'
/// `telegram.pid` fixture.
///
/// Why (#3516): the previous implementation hand-rolled "uniqueness" from
/// `std::process::id()` (constant across every thread of this ONE test
/// binary) plus a nanosecond timestamp. At very high `--test-threads`
/// (`cargo test -p trusty-agents --lib -- --test-threads=64`, ~4x realistic
/// CI concurrency), several of these pid-guard tests can start close enough
/// together that two threads observe the SAME nanosecond value — the
/// underlying clock's actual tick resolution is not guaranteed to be as
/// fine as `SystemTime`'s nanosecond formatting suggests, especially under
/// heavy scheduler contention. A collision means two tests silently share
/// one `telegram.pid` directory: one test's `TelegramPidGuard::acquire`
/// (over)writes or removes the file while another concurrently-running
/// test is asserting on it, producing an intermittent failure that looks
/// like a "process-liveness" bug but is actually a shared-path collision in
/// the test's OWN fixture, not the guard logic. `tempfile::tempdir()`
/// (already a dev-dependency of this crate, used throughout
/// `interaction_log.rs`/`build_info.rs`/etc.) creates its directory with an
/// OS-level atomic, retry-on-collision unique name — removing the
/// possibility of two calls ever returning the same path, without adding
/// any synchronisation between the tests.
/// What: `.keep()` (mirrors the same idiom already used by
/// `trusty-mpm`'s own tests, e.g. `manager_cli_client.rs`) leaks the
/// directory rather than auto-cleaning on drop, matching this helper's
/// previous behaviour exactly (callers already didn't clean up either).
fn tempdir_for_test() -> PathBuf {
    tempfile::tempdir()
        .expect("create unique tempdir for telegram pid-guard test")
        .keep()
}

// ── `/switch` persona pre-check resolution (directory-package fix) ──────────
//
// Why: the Telegram `/switch <name>` pre-check must accept the SAME persona
// forms the real dispatch resolver `AgentConfig::by_name_async` does — a
// directory package (`<name>/agent.toml`) as well as a flat `<name>.toml`.
// The canonical `assistant` persona ships package-only, so a flat-only
// pre-check regressed `/switch assistant` to "Unknown persona".

/// Create `<agents_dir>/<name>/agent.toml` (+ a minimal `persona.md`), the
/// directory-package layout `load_agent_package` resolves.
fn write_package_persona(agents_dir: &std::path::Path, name: &str) {
    let pkg = agents_dir.join(name);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
    std::fs::write(pkg.join("persona.md"), "persona").unwrap();
}

/// Why: the shared core of BOTH `/switch` pre-check tiers (project + `$HOME`)
/// must accept a directory package, not just a flat `.toml` — the exact gap
/// that broke `/switch assistant`.
/// What: a package-only persona and a flat persona each resolve `true`.
#[test]
fn persona_exists_in_accepts_directory_package() {
    let agents_dir = tempdir_for_test();
    write_package_persona(&agents_dir, "assistant");
    std::fs::write(agents_dir.join("izzie.toml"), "[agent]\nname = \"izzie\"\n").unwrap();

    assert!(
        super::persona_exists_in(&agents_dir, "assistant"),
        "directory-package persona (assistant/agent.toml) must resolve"
    );
    assert!(
        super::persona_exists_in(&agents_dir, "izzie"),
        "flat persona (izzie.toml) must still resolve"
    );
}

/// Why: a genuinely unknown name must still be rejected so `/switch` reports
/// "Unknown persona" rather than storing an unresolvable persona.
/// What: neither a flat file nor a package dir exists → `false`.
#[test]
fn persona_exists_in_rejects_unknown_name() {
    let agents_dir = tempdir_for_test();
    assert!(!super::persona_exists_in(&agents_dir, "does-not-exist"));
}

/// Why: `load_agent_package` reads `persona.md` unconditionally, so a package
/// with `agent.toml` but no `persona.md` would pass a naive pre-check and then
/// hard-fail on the next turn's load — the exact failure the pre-check guards
/// against. It must be rejected here.
/// What: an `agent.toml`-only package (no `persona.md`) resolves `false`.
#[test]
fn persona_exists_in_rejects_package_missing_persona_md() {
    let agents_dir = tempdir_for_test();
    let pkg = agents_dir.join("assistant");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
    // No persona.md written.
    assert!(
        !super::persona_exists_in(&agents_dir, "assistant"),
        "an incomplete package (agent.toml without persona.md) must NOT validate"
    );
}

/// Why: the project-local tier of the `/switch` pre-check (checked before the
/// `$HOME` fallback) is the one that resolves a repo's canonical `assistant`
/// package — the demo path.
/// What: a project whose `.trusty-agents/agents/assistant/` is a package
/// resolves `true`.
#[test]
fn project_persona_exists_accepts_directory_package() {
    let project = tempdir_for_test();
    let agents_dir = project.join(".trusty-agents").join("agents");
    write_package_persona(&agents_dir, "assistant");

    assert!(
        super::project_persona_exists(&project, "assistant"),
        "package-only assistant persona must validate for /switch"
    );
}

/// Why: negative guard for the project tier — an unknown name is rejected.
/// What: an empty project resolves `false`.
#[test]
fn project_persona_exists_rejects_unknown_name() {
    let project = tempdir_for_test();
    assert!(!super::project_persona_exists(&project, "nope"));
}

/// #4652 regression: the `/switch` intercept returns early from
/// `handle_message`, so a persona switch used to be dropped from the
/// last-human-turn clock. A paired human typing `/switch` is attending.
#[test]
fn switch_command_still_records_a_human_turn() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = crate::attendance::attendance_root(dir.path());
    let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 8, 3, 12, 0, 0).unwrap();

    assert!(
        super::handlers::note_turn_and_is_switch(
            Some(&root),
            "izzie",
            "/switch cto-assistant",
            now
        ),
        "`/switch` must still route to the switch handler"
    );

    let tracker = crate::attendance::AttendanceTracker::new(
        &root,
        crate::attendance::AttendanceConfig::default(),
    );
    let id = crate::assistants::AssistantInstanceId::new("izzie").expect("valid id");
    assert_eq!(
        tracker.last_human_turn(&id).expect("read"),
        Some(now),
        "a human typing `/switch` is attending; the turn must be recorded"
    );
}

/// #4683 regression: `handle_command` — the dispatch path for `/start`,
/// `/pair`, `/help`, `/connect`, `/clear`, `/status` — recorded no attendance.
/// A paired human polling `/status` every few minutes while a long task ran was
/// invisible to the clock and read as unattended after fifteen minutes, which
/// is the exact false positive this feature exists to prevent.
#[test]
fn paired_slash_command_records_a_human_turn() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = crate::attendance::attendance_root(dir.path());
    let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 8, 3, 12, 0, 0).unwrap();

    assert!(
        crate::attendance::note_command_turn_in(
            Some(&root),
            "izzie",
            crate::attendance::TurnOrigin::Human,
            true,
            now
        ),
        "a paired chat's slash command is a human turn"
    );

    let tracker = crate::attendance::AttendanceTracker::new(
        &root,
        crate::attendance::AttendanceConfig::default(),
    );
    let id = crate::assistants::AssistantInstanceId::new("izzie").expect("valid id");
    assert_eq!(tracker.last_human_turn(&id).expect("read"), Some(now));
}

/// `/start` and `/pair` are reachable by ANY chat, so the record has to stay
/// behind the pairing gate — otherwise a stranger could attend someone else's
/// assistant and mute their notifications.
#[test]
fn unpaired_slash_command_records_nothing() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = crate::attendance::attendance_root(dir.path());
    let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 8, 3, 12, 0, 0).unwrap();

    assert!(!crate::attendance::note_command_turn_in(
        Some(&root),
        "izzie",
        crate::attendance::TurnOrigin::Human,
        false,
        now
    ));

    let tracker = crate::attendance::AttendanceTracker::new(
        &root,
        crate::attendance::AttendanceConfig::default(),
    );
    let id = crate::assistants::AssistantInstanceId::new("izzie").expect("valid id");
    assert_eq!(
        tracker.last_human_turn(&id).expect("read"),
        None,
        "an unpaired sender must not manufacture attendance"
    );
}

/// The ordinary path still records, and is not misread as a switch.
#[test]
fn plain_message_records_a_human_turn_and_is_not_a_switch() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = crate::attendance::attendance_root(dir.path());
    let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 8, 3, 12, 0, 0).unwrap();

    assert!(!super::handlers::note_turn_and_is_switch(
        Some(&root),
        "izzie",
        "what is the status of #4652?",
        now
    ));

    let tracker = crate::attendance::AttendanceTracker::new(
        &root,
        crate::attendance::AttendanceConfig::default(),
    );
    let id = crate::assistants::AssistantInstanceId::new("izzie").expect("valid id");
    assert_eq!(tracker.last_human_turn(&id).expect("read"), Some(now));
}

// ---------------------------------------------------------------------------
// #4703: the two PRODUCTION entry points. The tests above exercise the
// injectable helpers UNDERNEATH `handle_command` / `handle_message`; these two
// drive the handlers themselves. Neither could exist before: both handlers
// resolved `$HOME` inline, so the natural assertion could only be made against
// the developer's real `~/.trusty-agents/attendance/<persona>.json`.
// ---------------------------------------------------------------------------

/// A persona no roster can resolve, so any dispatch that reaches the agent
/// loader fails on a filesystem read rather than an LLM call.
const PROBE_PERSONA: &str = "attendance-probe-4703";

/// A `Bot` whose API base is a closed loopback port.
///
/// Why: these tests drive real handlers, and real handlers reply. Pointing the
/// bot at `127.0.0.1:1` makes every send fail INSTANTLY with a connection
/// refusal instead of reaching api.telegram.org — hermetic, and no slower than
/// a local socket error. Every send in the paths below is either ignored or
/// returned, and the attendance write happens before any of them.
fn probe_bot() -> teloxide::Bot {
    teloxide::Bot::new("4703:probe")
        .set_api_url(reqwest::Url::parse("http://127.0.0.1:1/").expect("closed-port url parses"))
}

/// A minimal inbound Telegram message, built the way Telegram itself delivers
/// one — by deserializing the update payload.
fn probe_message(chat_id: i64, text: &str) -> teloxide::types::Message {
    serde_json::from_value(serde_json::json!({
        "message_id": 1,
        "date": 0,
        "chat": { "id": chat_id, "type": "private" },
        "from": { "id": 4703, "is_bot": false, "first_name": "Probe" },
        "text": text,
    }))
    .expect("valid telegram message payload")
}

/// A session map holding one chat already switched to [`PROBE_PERSONA`], so
/// both handlers record against a distinctive name rather than the `ctrl`
/// default.
fn probe_sessions(chat_id: ChatId, project_path: &std::path::Path) -> super::SessionMap {
    let mut session = super::ChatSession::new(project_path.to_path_buf());
    session.active_persona = Some(PROBE_PERSONA.to_string());
    let mut map = HashMap::new();
    map.insert(chat_id, session);
    Arc::new(tokio::sync::Mutex::new(map))
}

/// The last human turn recorded for `persona` under `root`.
fn recorded_turn(root: &std::path::Path, persona: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let tracker = crate::attendance::AttendanceTracker::new(
        root,
        crate::attendance::AttendanceConfig::default(),
    );
    let id = crate::assistants::AssistantInstanceId::new(persona).expect("valid id");
    tracker.last_human_turn(&id).expect("read")
}

/// #4703 regression: `handle_command` records attendance under the root it is
/// GIVEN, not one it resolves from `$HOME`.
///
/// Why: the assertion the old inline `default_attendance_root()` made
/// impossible. Deleting the `note_command_turn_in(...)` call from
/// `handle_command` must fail this test — confirmed by doing exactly that.
/// What: drives the real handler with `/clear` on a paired chat. The reply
/// fails against the closed-port bot; the hook ran before it.
#[tokio::test]
async fn telegram_handle_command_records_attendance_under_the_injected_root() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = Arc::new(crate::attendance::attendance_root(dir.path()));
    let chat_id = ChatId(4703);

    let paired: PairedChats = Arc::new(RwLock::new(HashMap::new()));
    paired.write().await.insert(chat_id, Instant::now());

    let _ = super::handlers::handle_command(
        probe_bot(),
        probe_message(chat_id.0, "/clear"),
        super::handlers::Command::Clear,
        probe_sessions(chat_id, dir.path()),
        Arc::new(dir.path().to_path_buf()),
        paired,
        Arc::new(dir.path().join("paired.json")),
        new_pending_pairs(),
        Some(Arc::clone(&root)),
    )
    .await;

    assert!(
        recorded_turn(&root, PROBE_PERSONA).is_some(),
        "handle_command must record the human turn under the INJECTED root; \
         finding nothing here means the hook resolved $HOME again (#4703)"
    );
}

/// #4703 regression: `handle_message` records attendance under the injected
/// root.
///
/// Why: same trap, the other Telegram entry point. Deleting the
/// `note_turn_and_is_switch(...)` call from `handle_message` must fail this
/// test — confirmed by doing exactly that.
/// What: drives the real handler with a `/switch` line on a paired chat.
/// `/switch` is intercepted right after the hook, so the handler returns
/// without ever reaching the LLM dispatch — and the hook deliberately runs
/// BEFORE that intercept (#4652), which is precisely what this pins.
#[tokio::test]
async fn telegram_handle_message_records_attendance_under_the_injected_root() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = Arc::new(crate::attendance::attendance_root(dir.path()));
    let chat_id = ChatId(4703);

    let paired: PairedChats = Arc::new(RwLock::new(HashMap::new()));
    paired.write().await.insert(chat_id, Instant::now());

    let _ = super::handlers::handle_message(
        probe_bot(),
        probe_message(chat_id.0, "/switch ctrl"),
        probe_sessions(chat_id, dir.path()),
        Arc::new(dir.path().to_path_buf()),
        paired,
        Some(Arc::clone(&root)),
        // #8190: an unsupervised host-wide gateway, the pre-#8190 shape.
        None,
    )
    .await;

    assert!(
        recorded_turn(&root, PROBE_PERSONA).is_some(),
        "handle_message must record the human turn under the INJECTED root; \
         finding nothing here means the hook resolved $HOME again (#4703)"
    );
}

// ---------------------------------------------------------------------------
// #8190 round-2 finding 2 (HIGH): `owners` used to scope the BINDING lane only.
// Everything below drives the two unscoped paths it left open — the `/switch`
// intercept and the `ChatSession` fallback — against a stand-in Telegram, so a
// reply is an assertion rather than a connection refusal.
// ---------------------------------------------------------------------------

/// A stand-in Telegram recording every `sendMessage` body, and a `Bot` aimed
/// at it.
///
/// Why: `probe_bot` points at a closed port, which proves only that a handler
/// TRIED to reply. The refusal these tests pin is a specific sentence, and the
/// fallback refusal is the ABSENCE of any request at all — both need a server
/// that answers.
async fn recording_telegram() -> (teloxide::Bot, Arc<std::sync::Mutex<Vec<String>>>) {
    let sent: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = Arc::clone(&sent);
    // A catch-all rather than `/bot<token>/sendMessage`: a handler that called
    // an unexpected Bot API method would otherwise show up as a 404 with an
    // empty body, which reads as a JSON-parse bug rather than as the extra call
    // it is. Recording `<path> <body>` keeps both assertable.
    let app = axum::Router::new().route(
        "/{*method}",
        axum::routing::post(move |uri: axum::http::Uri, body: String| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("{} {body}", uri.path()));
                axum::Json(serde_json::json!({
                    "ok": true,
                    "result": {
                        "message_id": 7427, "date": 0,
                        "chat": {"id": 8190, "type": "private"}
                    }
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let bot = teloxide::Bot::new("8190:owners")
        .set_api_url(reqwest::Url::parse(&format!("http://{addr}/")).expect("stand-in url parses"));
    (bot, sent)
}

/// Everything recorded by a stand-in Telegram so far.
fn recorded(sent: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    sent.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Write `<project>/.trusty-agents/agents/<name>/` as a complete package, so
/// `/switch <name>` resolves without touching the developer's real `$HOME`.
fn seed_project_persona(project: &std::path::Path, name: &str) {
    let dir = project.join(".trusty-agents").join("agents").join(name);
    std::fs::create_dir_all(&dir).expect("persona dir");
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = \"{name}\"\n"),
    )
    .expect("agent.toml");
    std::fs::write(dir.join("persona.md"), "# persona\n").expect("persona.md");
}

/// #8190 round-2 finding 2 (HIGH): a supervised per-assistant bot must not let
/// an update reach an assistant that owns no binding on it.
///
/// Why: `owners` narrowed `inbound::route` alone. An update that route declined
/// fell through to `handle_message`, which is unscoped — so a chat paired on
/// THIS bot's own pairing file could `/switch cto-assistant` and drive an
/// assistant bound only to another bot, and any plain-text message afterwards
/// ran as that persona in a gateway session the binding never authorized. Both
/// are the cross-assistant reach SPEC-AGENTS-09 T-2/T-4 forbid.
/// What: five real handler runs against a stand-in Telegram — a refused
/// `/switch`, an admitted one, and the fallback a supervised bot no longer
/// takes. The unsupervised (`owners: None`) bot is exercised beside each, so
/// the assertions are a narrowing and not a blanket refusal.
///
/// Pre-change this fails twice: `handle_switch` takes no owners and stores
/// `cto-assistant` on the session, and `handle_plain_text` does not exist — the
/// closure it replaces always reaches `handle_message`, which answers the
/// "Not paired" line the last assertion requires to be absent.
#[allow(
    clippy::await_holding_lock,
    reason = "the crate's own $HOME-sandboxing convention; see test_env::lock_home"
)]
#[tokio::test]
async fn telegram_gateway_fallback_session_cannot_switch_outside_the_owners() {
    let _home_guard = crate::test_env::lock_home();
    let home = tempfile::TempDir::new().expect("temp home");
    // SAFETY: `lock_home` is held for the whole body, the crate's convention.
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    // An EMPTY roster: `inbound::route` must find no binding for this chat, so
    // what happens next is the fallback decision under test and not a claim.
    // Without the pin, `agents_dir_candidates` reads the repo's own bundled
    // assistants from the CWD-relative tier.
    let _agents_dir = crate::channels::credentials::test_env::EnvVarGuard::set(
        "TAGENT_CONFIG_DIR",
        home.path()
            .join(".trusty-agents/agents")
            .to_str()
            .expect("utf-8 tempdir"),
    );
    let project = tempfile::TempDir::new().expect("temp project");
    for name in ["izzie", "cto-assistant"] {
        seed_project_persona(project.path(), name);
    }
    let project_path = Arc::new(project.path().to_path_buf());
    let owners: super::handlers::BotOwners = Some(Arc::new(vec!["izzie".to_string()]));
    let scope = owners.as_deref().map(Vec::as_slice);
    let chat_id = ChatId(8190);

    // 1. A `/switch` to an assistant that owns no binding on this bot is
    //    refused, and the session keeps whatever it had.
    let (bot, sent) = recording_telegram().await;
    let sessions: super::SessionMap = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    super::handlers::handle_switch(
        &bot,
        chat_id,
        &sessions,
        &project_path,
        "/switch cto",
        scope,
    )
    .await
    .expect("the refusal is delivered");
    let refusal = recorded(&sent);
    assert_eq!(refusal.len(), 1, "one reply: {refusal:?}");
    assert!(
        refusal[0].contains("owns no channel binding"),
        "the alias `cto` must be canonicalized and then refused: {refusal:?}"
    );
    assert!(
        sessions.lock().await.get(&chat_id).is_none(),
        "a refused switch must store nothing on the session"
    );

    // 2. An assistant that DOES own a binding on this bot still switches.
    let (bot, sent) = recording_telegram().await;
    super::handlers::handle_switch(
        &bot,
        chat_id,
        &sessions,
        &project_path,
        "/switch izzie",
        scope,
    )
    .await
    .expect("the confirmation is delivered");
    assert!(
        recorded(&sent)[0].contains("Switched to izzie"),
        "an owner must still be switchable: {:?}",
        recorded(&sent)
    );
    assert_eq!(
        sessions
            .lock()
            .await
            .get(&chat_id)
            .and_then(|s| s.active_persona.clone()),
        Some("izzie".to_string())
    );

    // 3. The same refused switch on an UNSUPERVISED bot is not refused: the
    //    gate belongs to per-assistant bots, not to `/switch` in general.
    let (bot, sent) = recording_telegram().await;
    let host_wide: super::SessionMap = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    super::handlers::handle_switch(
        &bot,
        chat_id,
        &host_wide,
        &project_path,
        "/switch cto",
        None,
    )
    .await
    .expect("the confirmation is delivered");
    assert!(
        recorded(&sent)[0].contains("Switched to cto"),
        "the pre-#8190 host-wide gateway keeps its any-persona `/switch`: {:?}",
        recorded(&sent)
    );

    // 4. A plain-text update no binding claims reaches NO gateway session on a
    //    supervised bot — not even the pairing refusal, which is the cheapest
    //    observable proof the fallback was never entered.
    let (bot, sent) = recording_telegram().await;
    super::handlers::handle_plain_text(
        bot,
        probe_message(chat_id.0, "move the 3pm"),
        Arc::clone(&sessions),
        Arc::clone(&project_path),
        Arc::new(RwLock::new(HashMap::new())),
        None,
        owners.clone(),
    )
    .await
    .expect("a dropped update is not an error");
    assert!(
        recorded(&sent).is_empty(),
        "a supervised bot has no gateway session to fall back to: {:?}",
        recorded(&sent)
    );

    // 5. The same update on an unsupervised bot DOES reach the gateway session,
    //    which answers with its pairing gate — so assertion 4 is the owners
    //    rule and not a broken handler.
    let (bot, sent) = recording_telegram().await;
    super::handlers::handle_plain_text(
        bot,
        probe_message(chat_id.0, "move the 3pm"),
        Arc::clone(&host_wide),
        Arc::clone(&project_path),
        Arc::new(RwLock::new(HashMap::new())),
        None,
        None,
    )
    .await
    .expect("the pairing refusal is delivered");
    let replies = recorded(&sent);
    assert!(
        replies.len() == 1 && replies[0].contains("Not paired"),
        "the pre-#8190 fallback still runs for a host-wide bot: {replies:?}"
    );
}
