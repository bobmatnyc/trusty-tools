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
    save_paired_chats, telegram_pid_alive, verify_pair_attempt,
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

/// Single-instance guard: our own PID must report as alive.
#[test]
fn telegram_pid_alive_true_for_self() {
    let self_pid = std::process::id() as i32;
    assert!(telegram_pid_alive(self_pid));
}

/// Single-instance guard: an implausible PID must report as dead.
///
/// Why: `acquire` relies on this to distinguish a live peer from a stale
/// lock. PID 0x7FFF_FFFF is far beyond any real PID, so `kill(pid, 0)`
/// fails with ESRCH.
#[test]
fn telegram_pid_alive_false_for_absurd_pid() {
    assert!(!telegram_pid_alive(i32::MAX));
}

/// Single-instance guard: `acquire` writes the current PID and `Drop`
/// removes the file.
#[test]
fn telegram_pid_guard_acquire_writes_and_drops() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram.pid");
    {
        let _guard = TelegramPidGuard::acquire(path.clone()).expect("acquire");
        let contents = std::fs::read_to_string(&path).expect("pid file exists");
        assert_eq!(contents.trim(), std::process::id().to_string());
    }
    // Guard dropped: file must be gone.
    assert!(!path.exists(), "PID file should be removed on drop");
}

/// Single-instance guard: a stale PID file (dead process) is overwritten,
/// not treated as a live conflict.
#[test]
fn telegram_pid_guard_stale_is_overwritten() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram.pid");
    // Write an absurd, definitely-dead PID.
    std::fs::write(&path, i32::MAX.to_string()).unwrap();
    let _guard = TelegramPidGuard::acquire(path.clone()).expect("stale lock should be reclaimed");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents.trim(), std::process::id().to_string());
}

/// Single-instance guard: a live PID file (our own PID) blocks acquire.
///
/// Why: This is the core protection — a second daemon must refuse to
/// start while a peer is alive. We use our own PID as a stand-in for a
/// running peer since it is guaranteed alive for the test's duration.
#[test]
fn telegram_pid_guard_live_conflict_is_rejected() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram.pid");
    std::fs::write(&path, std::process::id().to_string()).unwrap();
    let result = TelegramPidGuard::acquire(path.clone());
    assert!(result.is_err(), "live peer must block acquire");
    // The pre-existing file must be left intact for the live peer.
    assert!(path.exists());
}

/// Single-instance guard: unparseable PID file contents are treated as
/// stale and overwritten.
#[test]
fn telegram_pid_guard_garbage_is_overwritten() {
    let tmp = tempdir_for_test();
    let path = tmp.join("telegram.pid");
    std::fs::write(&path, "not-a-pid").unwrap();
    let _guard = TelegramPidGuard::acquire(path.clone()).expect("garbage lock should be reclaimed");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents.trim(), std::process::id().to_string());
}

/// Tiny helper that creates a unique tempdir under the system temp.
/// Why: Avoids pulling in the `tempfile` crate just for two tests.
fn tempdir_for_test() -> PathBuf {
    let uniq = format!(
        "open-mpm-telegram-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(uniq);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
