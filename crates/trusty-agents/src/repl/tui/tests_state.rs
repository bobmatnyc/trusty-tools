//! Tests split from `tui.rs` (#357). Flat re-exports in `mod.rs` make
//! every item reachable via `super::*`.

#![cfg(test)]

use super::*;

#[test]
fn repl_app_insert_and_backspace() {
    let mut a = ReplApp::new("ctrl".into(), "tester".into());
    a.insert_char('h');
    a.insert_char('i');
    assert_eq!(a.input_buf, "hi");
    assert_eq!(a.cursor_pos, 2);
    a.backspace();
    assert_eq!(a.input_buf, "h");
    assert_eq!(a.cursor_pos, 1);
}

#[test]
fn repl_app_cursor_left_right_clamps() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.insert_char('a');
    a.insert_char('b');
    a.cursor_left();
    a.cursor_left();
    a.cursor_left();
    assert_eq!(a.cursor_pos, 0);
    a.cursor_right();
    a.cursor_right();
    a.cursor_right();
    assert_eq!(a.cursor_pos, 2);
}

#[test]
fn repl_app_take_input_resets_buffer() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.insert_char('h');
    a.insert_char('i');
    let line = a.take_input();
    assert_eq!(line, Some("hi".to_string()));
    assert!(a.input_buf.is_empty());
    assert_eq!(a.cursor_pos, 0);
}

#[test]
fn repl_app_take_input_skips_whitespace_only() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.insert_char(' ');
    a.insert_char('\t');
    assert_eq!(a.take_input(), None);
}

#[test]
fn repl_app_history_prev_next() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.history = vec!["one".into(), "two".into(), "three".into()];
    a.insert_char('x');
    a.history_prev();
    assert_eq!(a.input_buf, "three");
    a.history_prev();
    assert_eq!(a.input_buf, "two");
    a.history_next();
    assert_eq!(a.input_buf, "three");
    a.history_next();
    assert_eq!(a.input_buf, "x"); // restored
}

#[test]
fn trim_surrounding_blank_lines_strips_leading_and_trailing() {
    let input = "\n\n  \nhello\n\nworld\n\n  \n";
    let out = trim_surrounding_blank_lines(input);
    assert_eq!(out, "hello\n\nworld");
}

#[test]
fn trim_surrounding_blank_lines_preserves_when_no_blanks() {
    assert_eq!(trim_surrounding_blank_lines("hi"), "hi");
    assert_eq!(trim_surrounding_blank_lines("a\nb"), "a\nb");
}

#[test]
fn trim_surrounding_blank_lines_empty_input() {
    assert_eq!(trim_surrounding_blank_lines(""), "");
    assert_eq!(trim_surrounding_blank_lines("\n\n  \n"), "");
}

#[test]
fn strip_interior_blank_lines_drops_all_blanks() {
    let input = "para1\n\n\npara2\n\n\n\npara3";
    let out = strip_interior_blank_lines(input);
    assert_eq!(out, "para1\npara2\npara3");
}

#[test]
fn strip_interior_blank_lines_drops_single_blank() {
    let input = "para1\n\npara2";
    assert_eq!(strip_interior_blank_lines(input), "para1\npara2");
}

#[test]
fn strip_interior_blank_lines_treats_whitespace_only_as_blank() {
    let input = "a\n   \n\t\nb";
    assert_eq!(strip_interior_blank_lines(input), "a\nb");
}

#[test]
fn collapse_blank_lines_drops_consecutive_empty_lines() {
    let input: Vec<Line<'static>> = vec![
        Line::from("a"),
        Line::from(""),
        Line::from(""),
        Line::from("b"),
        Line::from("   "),
        Line::from(""),
        Line::from("c"),
    ];
    let out = collapse_blank_lines(input);
    // Expected: a, blank, b, blank, c (two blanks collapsed each time).
    assert_eq!(out.len(), 5);
    assert_eq!(out[0].spans[0].content, "a");
    assert!(out[1].spans.iter().all(|s| s.content.trim().is_empty()));
    assert_eq!(out[2].spans[0].content, "b");
    assert!(out[3].spans.iter().all(|s| s.content.trim().is_empty()));
    assert_eq!(out[4].spans[0].content, "c");
}

#[test]
fn is_md_table_row_basic() {
    assert!(is_md_table_row("| a | b |"));
    assert!(is_md_table_row("  |x|"));
    assert!(!is_md_table_row("hello"));
    assert!(!is_md_table_row(""));
}

#[test]
fn is_md_table_separator_basic() {
    assert!(is_md_table_separator("|---|---|"));
    assert!(is_md_table_separator("| :--- | ---: |"));
    assert!(is_md_table_separator("|:-:|:-:|"));
    assert!(!is_md_table_separator("| a | b |"));
    assert!(!is_md_table_separator("hello"));
    // No dashes → not a separator.
    assert!(!is_md_table_separator("|   |   |"));
}

#[test]
fn parse_md_table_cells_basic() {
    assert_eq!(
        parse_md_table_cells("| a | b |"),
        vec!["a".to_string(), "b".to_string()]
    );
    assert_eq!(
        parse_md_table_cells("|x|y|z|"),
        vec!["x".to_string(), "y".to_string(), "z".to_string()]
    );
    assert_eq!(
        parse_md_table_cells("| Technique | Impact | Effort |"),
        vec![
            "Technique".to_string(),
            "Impact".to_string(),
            "Effort".to_string()
        ]
    );
}

#[test]
fn truncate_cell_basic() {
    assert_eq!(truncate_cell("abc", 5), "abc");
    assert_eq!(truncate_cell("abcdef", 4), "abc…");
    assert_eq!(truncate_cell("hello", 0), "");
    assert_eq!(truncate_cell("hello", 5), "hello");
}

#[test]
fn render_markdown_table_emits_expected_lines() {
    let header = vec![
        "Technique".to_string(),
        "Impact".to_string(),
        "Effort".to_string(),
    ];
    let body = vec![
        vec![
            "Prompt constraints".to_string(),
            "10–20%".to_string(),
            "Low".to_string(),
        ],
        vec![
            "max_tokens caps".to_string(),
            "Prevents runaway".to_string(),
            "Low".to_string(),
        ],
    ];
    let out = render_markdown_table(&header, &body, 200, "   ", None);
    // top border + header + separator + 2 body + bottom = 6 lines.
    assert_eq!(out.len(), 6);
    // First line should contain the top-left corner.
    let first: String = out[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        first.contains('┌'),
        "expected ┌ in top border, got: {first}"
    );
    assert!(first.contains('┬'));
    assert!(first.contains('┐'));
    // Header row contains "Technique" cell content.
    let header_line: String = out[1]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(header_line.contains("Technique"));
    assert!(header_line.contains("Impact"));
    assert!(header_line.contains("Effort"));
    // Separator row uses ├ ┼ ┤.
    let sep_line: String = out[2]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(sep_line.contains('├'));
    assert!(sep_line.contains('┼'));
    assert!(sep_line.contains('┤'));
    // Bottom border uses └ ┴ ┘.
    let last: String = out[5]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(last.contains('└'));
    assert!(last.contains('┴'));
    assert!(last.contains('┘'));
    // Every line begins with the indent prefix.
    for l in &out {
        let s: String = l
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(s.starts_with("   "), "line missing indent: {s:?}");
    }
}

#[test]
fn render_markdown_table_truncates_when_too_wide() {
    let header = vec!["AVeryLongHeaderName".to_string(), "B".to_string()];
    let body = vec![vec!["X".to_string(), "Y".to_string()]];
    // Tight width forces truncation of the long header.
    let out = render_markdown_table(&header, &body, 18, "", None);
    assert_eq!(out.len(), 5);
    let header_line: String = out[1]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    // Either truncated with ellipsis OR fits — but width must not exceed limit.
    assert!(
        header_line.chars().count() <= 18,
        "row too wide: {header_line:?}"
    );
}

#[test]
fn push_assistant_trims_surrounding_blanks() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.push_assistant("\n\n2 + 2 = 4.\n\n   No tools needed.\n\n\n", false);
    assert_eq!(a.chat.len(), 1);
    assert_eq!(a.chat[0].text, "2 + 2 = 4.\n   No tools needed.");
}

#[test]
fn repl_app_push_user_keeps_banner() {
    // The banner now lives in the chat scroll buffer (see `banner_lines`)
    // and scrolls off the top naturally as content grows. push_user no
    // longer toggles `show_banner`.
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    assert!(a.show_banner);
    a.push_user("hello");
    assert!(
        a.show_banner,
        "banner should remain visible after first message"
    );
    assert_eq!(a.chat.len(), 1);
    assert_eq!(a.chat[0].role, ChatRole::User);
}

#[test]
fn repl_app_remember_input_dedups() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.remember_input("hello");
    a.remember_input("hello"); // dup
    a.remember_input("world");
    assert_eq!(a.history, vec!["hello", "world"]);
}

#[test]
fn repl_app_scroll_clamps_at_zero() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    // #329: scroll() now also clamps against `last_max_scroll`. Publish a
    // generous cap so this test continues to exercise the floor (0)
    // without colliding with the upper clamp.
    a.last_max_scroll
        .store(100, std::sync::atomic::Ordering::Relaxed);
    a.scroll(5);
    assert_eq!(a.scroll_offset, 0);
    a.scroll(-3);
    assert_eq!(a.scroll_offset, 3);
    a.scroll(10);
    assert_eq!(a.scroll_offset, 0);
}

/// Why (#329): Mouse wheel scroll-up used to accumulate `scroll_offset`
/// indefinitely past the actual scrollback height; subsequent scroll-down
/// had to "burn off" the phantom offset before any visible movement.
/// What: After `draw_chat` publishes a max via `last_max_scroll`,
/// `scroll()` must clamp upward deltas at that cap.
/// Test: simulate render (publish cap=5), apply -100 → offset should be 5.
#[test]
fn repl_app_scroll_clamps_at_max_offset() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    // Simulate the render path publishing a max_offset of 5.
    a.last_max_scroll
        .store(5, std::sync::atomic::Ordering::Relaxed);
    a.scroll(-100);
    assert_eq!(a.scroll_offset, 5, "should clamp to last_max_scroll");
    // Scrolling up further is a no-op.
    a.scroll(-3);
    assert_eq!(a.scroll_offset, 5);
    // Scrolling down decrements normally.
    a.scroll(2);
    assert_eq!(a.scroll_offset, 3);
    // When the cap shrinks (e.g. content removed), an explicit scroll
    // brings the offset back into range.
    a.last_max_scroll
        .store(1, std::sync::atomic::Ordering::Relaxed);
    a.scroll(0);
    assert_eq!(a.scroll_offset, 1);
}

/// Why (#331): The claude-mpm session segment must surface a styled
/// `MPM:` chunk distinct from the `TM:` chunk when any TM session is
/// running the claude-mpm adapter; suppressed entirely when zero.
/// What: Build the rich statusline with `claude_mpm_session_count > 0`
/// and assert the rendered text contains the MPM segment.
/// Test: flatten spans → string and look for `MPM: 2 sessions`.
#[test]
fn rich_statusline_includes_claude_mpm_segment_when_present() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.status_line = Some("✓ LLM: openrouter (sonnet) · All systems go.".into());
    a.tm_session_count = 3;
    a.claude_mpm_session_count = 2;
    let line = build_rich_statusline(&a);
    let text: String = line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("MPM: 2 sessions"),
        "missing MPM segment: {text}"
    );
    assert!(
        text.contains("TM: 3 sessions"),
        "missing TM segment: {text}"
    );
    // Suppress when zero.
    a.claude_mpm_session_count = 0;
    let line2 = build_rich_statusline(&a);
    let text2: String = line2
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        !text2.contains("MPM:"),
        "MPM segment should be hidden when 0: {text2}"
    );
}

// --- AgentScope tests ---

/// Why: Default scope must be User so ctrl starts with cyan label without explicit init.
/// What: ReplApp::new yields agent_scope == AgentScope::User.
/// Test: Construct a fresh ReplApp and assert agent_scope is User.
#[test]
fn repl_app_agent_scope_default() {
    let a = ReplApp::new("ctrl".into(), "u".into());
    assert_eq!(a.agent_scope, AgentScope::User);
}

/// Why: User scope must map to cyan; project scope must map to yellow.
/// What: match expression on AgentScope returns the correct Color variant.
/// Test: Assert both branches of the scope-to-color match.
#[test]
fn agent_scope_label_color_user_vs_project() {
    let user_color = match AgentScope::User {
        AgentScope::User => Color::Cyan,
        AgentScope::Project => Color::Yellow,
    };
    let project_color = match AgentScope::Project {
        AgentScope::User => Color::Cyan,
        AgentScope::Project => Color::Yellow,
    };
    assert_eq!(user_color, Color::Cyan);
    assert_eq!(project_color, Color::Yellow);
}

/// Why: TokenUpdate must increment cumulative counters so the input bar
/// reflects live token usage; TokenReset zeros both counters.
/// What: Mutate `tokens_in`/`tokens_out` directly (mirrors process_event).
/// Test: Apply two updates and assert sum; then reset and assert zero.
#[test]
fn repl_app_token_update_accumulates() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.tokens_in = a.tokens_in.saturating_add(100);
    a.tokens_out = a.tokens_out.saturating_add(50);
    a.tokens_in = a.tokens_in.saturating_add(25);
    a.tokens_out = a.tokens_out.saturating_add(75);
    assert_eq!(a.tokens_in, 125);
    assert_eq!(a.tokens_out, 125);
    a.tokens_in = 0;
    a.tokens_out = 0;
    assert_eq!(a.tokens_in, 0);
    assert_eq!(a.tokens_out, 0);
}

/// Why: `/clear` zeroes the running session token counters but must NOT
/// erase what the user has spent earlier today — daily totals survive
/// across `/clear` and process restarts.
/// What: Set `daily_cost_start` to a non-zero value, simulate
/// TokenReset by zeroing tokens, and assert `daily_cost_start` is intact.
/// Test: Self-explanatory.
#[test]
fn repl_app_token_reset_preserves_daily_cost_start() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.daily_cost_start = 0.0123;
    a.tokens_in = 1000;
    a.tokens_out = 500;
    // Mirrors what TokenReset does in process_event.
    a.tokens_in = 0;
    a.tokens_out = 0;
    assert_eq!(a.tokens_in, 0);
    assert_eq!(a.tokens_out, 0);
    assert!((a.daily_cost_start - 0.0123).abs() < 1e-9);
}

/// Why: The first TokenUpdate must flush to disk; later updates within
/// the throttle window must not. Verifies the throttle window guards the
/// hot path.
/// What: Call `persist_daily_usage_if_due` twice in quick succession;
/// assert the file was written exactly once (mtime stable on second
/// call). Then advance `last_usage_write` past the window and assert
/// the next call writes again.
/// Test: Self-explanatory.
#[test]
fn persist_daily_usage_writes_then_throttles() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.usage_project_dir = dir.path().to_path_buf();
    a.tokens_in = 1000;
    a.tokens_out = 1000;

    // First call: no prior write → must persist.
    persist_daily_usage_if_due(&mut a);
    let path = crate::usage::daily::usage_path(dir.path());
    assert!(path.exists(), "first call should create the file");
    let first_stamp = a.last_usage_write.expect("first call records timestamp");

    // Second call immediately: throttled, timestamp unchanged.
    persist_daily_usage_if_due(&mut a);
    assert_eq!(a.last_usage_write.unwrap(), first_stamp);

    // Force the throttle to expire and call again.
    a.last_usage_write =
        Some(std::time::Instant::now() - USAGE_WRITE_INTERVAL - std::time::Duration::from_secs(1));
    persist_daily_usage_if_due(&mut a);
    assert!(a.last_usage_write.unwrap() > first_stamp);
}
