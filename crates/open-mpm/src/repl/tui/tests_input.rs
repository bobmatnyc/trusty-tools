//! Tests split from `tui.rs` (#357). Flat re-exports in `mod.rs` make
//! every item reachable via `super::*`.

#![cfg(test)]

use super::*;

/// Why: The activity spinner must visibly cycle frame-to-frame so users
/// can tell the LLM is actively working. `tick_count` drives the index;
/// bumping it must change the leading glyph.
/// What: Render row1 at tick=0 and tick=1, assert the glyph differs.
/// Test: Direct comparison of leading character.
#[test]
fn activity_row1_spinner_animates_with_tick() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.tick_count = 0;
    let line0 = build_activity_row1(&a, 5, 120);
    let glyph0 = line0.spans[0].content.to_string();

    a.tick_count = 1;
    let line1 = build_activity_row1(&a, 5, 120);
    let glyph1 = line1.spans[0].content.to_string();

    assert_ne!(glyph0, glyph1, "spinner did not advance with tick_count");
    assert!(
        SPINNER_FRAMES.iter().any(|f| glyph0.starts_with(f)),
        "frame 0 not in animation set: {glyph0}"
    );
    assert!(
        SPINNER_FRAMES.iter().any(|f| glyph1.starts_with(f)),
        "frame 1 not in animation set: {glyph1}"
    );
}

/// Why: The rust-rainbow shimmer must flow across the spinner line as
/// `rainbow_tick` advances. Same character at different ticks must get
/// different colors, otherwise the effect is static.
/// What: Build rainbow_spans for "abc" at tick=0 and tick=1; assert that
/// the color of index 0 changes between the two ticks.
/// Test: Direct color comparison.
#[test]
fn rainbow_spans_advances_with_tick() {
    let s0 = rainbow_spans("abc", 0);
    let s1 = rainbow_spans("abc", 1);
    // Each tick shifts the gradient hue, so the same character index
    // must get a different color between consecutive ticks.
    assert_ne!(s0[0].style.fg, s1[0].style.fg, "rainbow did not flow");
    assert_ne!(s0[1].style.fg, s1[1].style.fg, "rainbow did not flow");
}

/// Why: `hsl_to_rgb` is the foundation of the smooth gradient; if the
/// conversion is wrong the entire shimmer is wrong. Spot-check the three
/// canonical edge cases that pin the algorithm.
/// What: white (l=1), black (l=0), pure red (h=0,s=1,l=0.5).
/// Test: Direct equality assertions.
#[test]
fn hsl_to_rgb_edge_cases() {
    assert_eq!(hsl_to_rgb(0.0, 0.0, 1.0), (255, 255, 255));
    assert_eq!(hsl_to_rgb(0.0, 0.0, 0.0), (0, 0, 0));
    assert_eq!(hsl_to_rgb(0.0, 1.0, 0.5), (255, 0, 0));
}

/// Why: `rainbow_spans` must emit one span per character so each glyph
/// can carry its own color — collapsing multiple chars into one span
/// would defeat the per-character flow.
/// What: Build for a 5-char string; assert 5 spans.
/// Test: Length check.
#[test]
fn rainbow_spans_one_span_per_char() {
    let spans = rainbow_spans("hello", 0);
    assert_eq!(spans.len(), 5);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(joined, "hello");
}

/// Why: When tokens have been streamed, row 1 should add the `↓ Nk
/// tokens` segment between elapsed and status word.
/// What: Set tokens_out and assert segment appears.
/// Test: substring presence.
#[test]
fn activity_row1_omits_tokens_when_zero() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.tokens_out = 2900;
    let line = build_activity_row1(&a, 138, 200);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("↓ 2.9k tokens"),
        "missing token segment: {text}"
    );
    assert!(text.contains("2m 18s"), "missing elapsed: {text}");
}

/// Why: When `status_line` is unset (defensive path) the rich statusline
/// must still render the `[open-mpm]` prefix so the row never goes blank.
/// What: Fresh app with no status_line → prefix appears, body is the
/// legacy segment string ("User" by default).
/// Test: Assert both substrings.
#[test]
fn rich_statusline_fallback_when_status_line_missing() {
    let a = ReplApp::new("ctrl".into(), "u".into());
    let line = build_rich_statusline(&a);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.starts_with("[open-mpm] "), "missing prefix: {text}");
    assert!(text.contains("User"), "missing fallback segment: {text}");
}

/// Why: `style_status_chunk` is the per-chunk styler; each known prefix
/// must produce the right span composition (count of spans + text).
/// What: Exercise each branch (LLM, count, success, unknown).
/// Test: Assert flattened text matches the input for each chunk.
#[test]
fn rich_statusline_chunks_styled() {
    let llm = style_status_chunk("✓ LLM: openrouter (m)");
    let llm_text: String = llm.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(llm_text, "✓ LLM: openrouter (m)");

    let ok = style_status_chunk("All systems go.");
    let ok_text: String = ok.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(ok_text, "All systems go.");

    // #293: token + cost chunks pass through normally.
    let tok = style_status_chunk("↑1.2k ↓0.8k");
    let tok_text: String = tok.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(tok_text, "↑1.2k ↓0.8k");

    let cost = style_status_chunk("$0.0034");
    let cost_text: String = cost.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(cost_text, "$0.0034");

    let unknown = style_status_chunk("anything else");
    let unknown_text: String = unknown.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(unknown_text, "anything else");
}

/// Why: `strip_vendor_prefix` powers the activity-row and statusline
/// model-display compaction. It must drop everything up to and including
/// the first `/`, leaving bare ids alone.
/// What: `anthropic/claude-haiku-4-5` → `claude-haiku-4-5`,
/// `openai/gpt-4o` → `gpt-4o`, `claude-haiku-4-5` → unchanged.
/// Test: Three cases.
#[test]
fn strip_vendor_prefix_strips_first_segment() {
    assert_eq!(
        strip_vendor_prefix("anthropic/claude-haiku-4-5"),
        "claude-haiku-4-5"
    );
    assert_eq!(strip_vendor_prefix("openai/gpt-4o"), "gpt-4o");
    assert_eq!(strip_vendor_prefix("claude-haiku-4-5"), "claude-haiku-4-5");
}

/// Why: Row 3 of the activity panel must NOT echo the cycling status
/// word from row 1. `is_redundant_thinking_step` collapses any
/// "thinking"/"working"/"processing" string (with or without trailing
/// dots/ellipses) to true so callers can blank the row.
/// What: Empty, "thinking", "Thinking…", "WORKING.", and "processing"
/// all redundant; meaningful step text ("reading file") is kept.
/// Test: Five redundant + one kept.
#[test]
fn is_redundant_thinking_step_collapses_status_words() {
    assert!(is_redundant_thinking_step(""));
    assert!(is_redundant_thinking_step("thinking"));
    assert!(is_redundant_thinking_step("Thinking…"));
    assert!(is_redundant_thinking_step("WORKING."));
    assert!(is_redundant_thinking_step("processing"));
    assert!(!is_redundant_thinking_step("reading file"));
}

/// Why: Up-arrow should restore `last_prompt` into the input buffer when
/// idle, so users can recall and edit/resubmit the most recent prompt
/// without re-typing.
/// What: With `last_prompt` set and `thinking == false`, KeyCode::Up
/// copies `last_prompt` into `input_buf` and does NOT set `pending_cancel`.
/// Test: Set last_prompt, send Up, assert input_buf matches and cancel
/// was NOT signaled.
#[test]
fn repl_app_up_arrow_recalls_last_prompt() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.last_prompt = "hello world".to_string();
    a.thinking = false;
    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(r, None);
    assert_eq!(a.input_buf, "hello world");
    assert_eq!(a.cursor_pos, "hello world".len());
    assert!(!a.pending_cancel, "idle Up must NOT signal cancel");
}

/// Why: Up-arrow while the LLM is busy must signal cancellation (so the
/// event loop can abort the JoinHandle) AND restore the last prompt for
/// editing.
/// What: With `thinking == true`, KeyCode::Up sets `pending_cancel` AND
/// copies `last_prompt` into `input_buf`.
/// Test: Set thinking + last_prompt, send Up, assert both effects.
#[test]
fn repl_app_up_arrow_when_busy_signals_cancel() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.last_prompt = "long task".to_string();
    a.thinking = true;
    let r = handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(r, None);
    assert!(a.pending_cancel, "busy Up must signal cancel");
    assert_eq!(a.input_buf, "long task", "must restore last_prompt");
}

/// Why: With no prior submission, Up-arrow has nothing to recall — it
/// must be a no-op rather than overwriting whatever the user has typed.
/// What: Empty `last_prompt`, non-empty input_buf → input_buf unchanged.
/// Test: Type chars, send Up with empty last_prompt, assert input_buf
/// preserved.
#[test]
fn repl_app_up_arrow_noop_when_no_last_prompt() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.insert_char('a');
    a.insert_char('b');
    a.last_prompt.clear();
    a.thinking = false;
    handle_key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(a.input_buf, "ab");
    assert!(!a.pending_cancel);
}

/// Why: `parse_token_count_from_step` should pull explicit `↓ N tokens`
/// values out of activity-step text so the input-row counter snaps to the
/// real number when an upstream emitter provides it (#298).
/// What: Verify k-suffix, plain digits, decimal-k, and the no-match cases.
/// Test: Pure function, table-driven assertions.
#[test]
fn parse_token_count_from_step_handles_common_shapes() {
    assert_eq!(parse_token_count_from_step("↓ 2.4k tokens"), Some(2400));
    assert_eq!(parse_token_count_from_step("↓2k tokens"), Some(2000));
    assert_eq!(parse_token_count_from_step("↓ 512 tokens"), Some(512));
    assert_eq!(
        parse_token_count_from_step("foo ↓ 100 tokens bar"),
        Some(100)
    );
    assert_eq!(parse_token_count_from_step("processing..."), None);
    assert_eq!(parse_token_count_from_step("↓ 5 lines"), None);
}

/// Why: `busy_since` is the activity panel's source of truth for the
/// elapsed timer + spinner cycling — it must default to None on a fresh
/// app so the chat fills the screen when idle.
/// What: Construct a fresh ReplApp, assert busy_since == None.
/// Test: Mechanical assertion.
#[test]
fn repl_app_busy_since_default_none() {
    let a = ReplApp::new("ctrl".into(), "u".into());
    assert!(a.busy_since.is_none());
    assert!(a.streaming_preview.is_empty());
}

/// Why: When LlmResponse arrives, the activity panel must collapse —
/// busy_since must clear so the layout swaps back to the idle chat-fills
/// layout, and streaming_preview must clear so it doesn't ghost.
/// What: Set busy_since + preview, then mirror the LlmResponse mutations
/// directly (mirrors process_event arm body), assert both are cleared.
/// Test: Direct state mutation; full event flow tested indirectly via tmux.
#[test]
fn repl_app_busy_since_and_preview_cleared_on_response() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.busy_since = Some(std::time::Instant::now());
    a.streaming_preview = "in flight".into();
    a.thinking = true;

    // Simulate LlmResponse handler.
    a.push_assistant("done", false);
    a.thinking = false;
    a.thinking_lines.clear();
    a.busy_since = None;
    a.streaming_preview.clear();

    assert!(a.busy_since.is_none());
    assert!(a.streaming_preview.is_empty());
    assert!(!a.thinking);
}

/// Why: `truncate_to` is the width-clamp helper used by the activity
/// panel's model + step rows — it must respect char boundaries (not byte)
/// so multi-byte glyphs don't get sliced.
/// What: Truncate a unicode-laden string to a small width and assert the
/// char count matches and no panic occurs.
/// Test: Pass-through case + truncation case.
#[test]
fn truncate_to_respects_char_boundaries() {
    assert_eq!(truncate_to("hello".to_string(), 10), "hello");
    assert_eq!(truncate_to("hello world".to_string(), 5), "hello");
    // Multi-byte: '⠋' is 3 bytes but 1 char.
    let s = "⠋⠙⠹⠸⠼".to_string();
    assert_eq!(truncate_to(s, 3).chars().count(), 3);
}

/// Why: AgentScopeChanged must update agent_scope so the next render picks
/// up the new color without any additional state wiring.
/// What: Simulate process_event logic by directly mutating app.agent_scope
///   (mirrors the handler body) and assert the new value.
/// Test: Start as User, set to Project, assert Project; then back to User.
#[tokio::test]
async fn repl_app_agent_scope_changed_event_updates_state() {
    let app = std::sync::Arc::new(tokio::sync::Mutex::new(ReplApp::new(
        "ctrl".into(),
        "u".into(),
    )));

    // Simulate handling AgentScopeChanged(Project).
    {
        let mut a = app.lock().await;
        a.agent_scope = AgentScope::Project;
    }
    assert_eq!(app.lock().await.agent_scope, AgentScope::Project);

    // Simulate handling AgentScopeChanged(User) (e.g. disconnect).
    {
        let mut a = app.lock().await;
        a.agent_scope = AgentScope::User;
    }
    assert_eq!(app.lock().await.agent_scope, AgentScope::User);
}

// === Fenced code-block tests (#321) ============================

#[test]
fn code_fence_lang_recognizes_openers_and_closers() {
    assert_eq!(code_fence_lang("```bash"), Some("bash".into()));
    assert_eq!(code_fence_lang("```sh"), Some("sh".into()));
    assert_eq!(code_fence_lang("```Rust"), Some("rust".into()));
    assert_eq!(code_fence_lang("```"), Some("".into()));
    assert_eq!(code_fence_lang("  ```bash  "), Some("bash".into()));
    assert_eq!(code_fence_lang("hello"), None);
    assert_eq!(code_fence_lang("``"), None);
}

#[test]
fn is_executable_shell_lang_matches_shells() {
    assert!(is_executable_shell_lang("bash"));
    assert!(is_executable_shell_lang("sh"));
    assert!(is_executable_shell_lang("zsh"));
    assert!(is_executable_shell_lang("fish"));
    assert!(!is_executable_shell_lang("rust"));
    assert!(!is_executable_shell_lang("python"));
    assert!(!is_executable_shell_lang(""));
}

#[test]
fn extract_last_shell_block_finds_bash() {
    let text = "Run this:\n```bash\necho hello\nls -la\n```\nDone.";
    assert_eq!(
        extract_last_shell_block(text),
        Some("echo hello\nls -la".into())
    );
}

#[test]
fn extract_last_shell_block_returns_last_when_multiple() {
    let text = "```bash\nfirst\n```\nstuff\n```sh\nsecond\nthird\n```\n";
    assert_eq!(extract_last_shell_block(text), Some("second\nthird".into()));
}

#[test]
fn extract_last_shell_block_ignores_non_shell() {
    let text = "```rust\nfn main() {}\n```";
    assert_eq!(extract_last_shell_block(text), None);
}

#[test]
fn extract_last_shell_block_none_without_block() {
    assert_eq!(extract_last_shell_block("plain prose only"), None);
}

#[test]
fn repl_app_last_bash_block_updates_on_push() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    assert_eq!(a.last_bash_block, None);
    a.push_assistant("Try `ls`:\n```bash\nls -la\n```", false);
    assert_eq!(a.last_bash_block, Some("ls -la".into()));
    // A non-shell block does not overwrite, but a fresh shell one does.
    a.push_assistant("Here:\n```sh\npwd\n```", false);
    assert_eq!(a.last_bash_block, Some("pwd".into()));
}

#[test]
fn repl_app_last_bash_block_skips_errors() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("```bash\nls\n```", false);
    a.push_assistant("```bash\nrm -rf /\n```", true); // error entry
    // Error entries are not used as the source of truth.
    assert_eq!(a.last_bash_block, Some("ls".into()));
}

#[test]
fn ctrl_e_pastes_last_bash_block_when_input_empty() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("```bash\necho hi\n```", false);
    let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
    let _ = handle_key(&mut a, key);
    assert_eq!(a.input_buf, "echo hi");
    assert_eq!(a.cursor_pos, "echo hi".len());
}

#[test]
fn ctrl_e_falls_back_to_end_of_line_when_input_nonempty() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("```bash\necho hi\n```", false);
    a.set_input("typed text".into());
    a.cursor_pos = 0;
    let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
    let _ = handle_key(&mut a, key);
    // Input unchanged; cursor moved to end (readline End-of-line).
    assert_eq!(a.input_buf, "typed text");
    assert_eq!(a.cursor_pos, "typed text".len());
}

#[test]
fn ctrl_e_no_op_when_no_block_and_input_empty() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
    let _ = handle_key(&mut a, key);
    assert_eq!(a.input_buf, "");
    assert_eq!(a.cursor_pos, 0);
}

// #326 Tests 3 & 4: extract_last_shell_block edge cases for unclosed
// fences. These cover truncation-at-max-tokens scenarios and malformed
// assistant output where a fence opens but never closes.

#[test]
fn extract_last_shell_block_unclosed_fence_returns_none() {
    // Simulates truncation at max_tokens — fence opened, never closed.
    // Without a closing fence we cannot know the block's intended end,
    // so we must return None rather than silently inferring EOF.
    let text = "Here's a script:\n```bash\necho hello\nls -la";
    assert_eq!(extract_last_shell_block(text), None);
}

#[test]
fn extract_last_shell_block_closed_then_unclosed_returns_closed() {
    // Complete block, then an unclosed one — should return the
    // completed block's content (the unclosed trailing block is ignored
    // for the same safety reason as the test above).
    let text = "```bash\necho first\n```\n```sh\nunclosed";
    assert_eq!(
        extract_last_shell_block(text),
        Some("echo first".to_string())
    );
}

// #326 Test 5: Ctrl+E must be a no-op when the only fenced block is a
// non-shell language (e.g. python). `last_bash_block` should remain None
// and the input/cursor must not change.
#[test]
fn ctrl_e_noop_when_python_block_but_no_shell_block() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("Result:\n```python\nprint('hi')\n```", false);
    // Python block → last_bash_block should be None.
    assert_eq!(a.last_bash_block, None);
    // Ctrl+E with empty input and no bash block: must not change input
    // or cursor.
    let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
    let _ = handle_key(&mut a, key);
    assert_eq!(a.input_buf, "");
    assert_eq!(a.cursor_pos, 0);
}
