//! Tests split from `tui.rs` (#357). Flat re-exports in `mod.rs` make
//! every item reachable via `super::*`.

#![cfg(test)]

use super::*;

/// Why: Cost rendering must use 4 decimals below $0.01 and 3 above so the
/// statusline stays readable across small (sub-cent) and larger costs.
/// What: Spot-check both branches.
/// Test: Self-explanatory.
#[test]
fn format_cost_value_thresholds() {
    assert_eq!(format_cost_value(0.0001), "$0.0001");
    assert_eq!(format_cost_value(0.5), "$0.500");
}

/// Why: Picker navigation must wrap from the last item back to the first
/// (and vice versa) so the user can reach any item with either arrow.
/// What: Down on the last index → 0; Up on index 0 → last index.
/// Test: Construct a 3-item picker, walk the indices.
#[test]
fn repl_app_picker_navigation_wraps() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.picker = Some(PickerState {
        items: vec!["a".into(), "b".into(), "c".into()],
        selected: 0,
        title: "T".into(),
        kind: PickerKind::Model,
    });
    // Up from 0 → wraps to last (2).
    let key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    handle_picker_key(&mut a, key);
    assert_eq!(a.picker.as_ref().unwrap().selected, 2);
    // Down from 2 → wraps to 0.
    let key = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_picker_key(&mut a, key);
    assert_eq!(a.picker.as_ref().unwrap().selected, 0);
    // Down 0→1.
    handle_picker_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(a.picker.as_ref().unwrap().selected, 1);
}

/// Why: Enter must close the picker and stash the selection so the event
/// loop can synthesize a `/model …` Submit.
/// What: Picker becomes None; `pending_picker_selection` gets `(kind, item)`.
/// Test: Open picker, simulate Enter on selected item, assert state.
#[test]
fn repl_app_picker_enter_sets_pending_selection() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.picker = Some(PickerState {
        items: vec![
            "anthropic/claude-haiku-4-5".into(),
            "anthropic/claude-sonnet-4-6".into(),
        ],
        selected: 1,
        title: "Select Model".into(),
        kind: PickerKind::Model,
    });
    handle_picker_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(a.picker.is_none());
    assert_eq!(
        a.pending_picker_selection,
        Some((PickerKind::Model, "anthropic/claude-sonnet-4-6".into()))
    );
}

/// Why: Esc must dismiss the picker without leaving any pending selection
/// — picker is purely a cancel, no state side-effects.
/// What: After Esc, both `picker` and `pending_picker_selection` are None.
/// Test: Open picker, press Esc, assert clean state.
#[test]
fn repl_app_picker_esc_dismisses() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.picker = Some(PickerState {
        items: vec!["x".into()],
        selected: 0,
        title: "T".into(),
        kind: PickerKind::Provider,
    });
    handle_picker_key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(a.picker.is_none());
    assert!(a.pending_picker_selection.is_none());
}

/// Why bug fix (#switch-popup): `/switch` (no arg) populates the inline
/// flat-list picker with `choices_context = Some("switch")`. Pressing
/// Enter on a selection must NOT insert the persona name into the input
/// buffer (legacy behavior); it must directly queue a synthetic
/// `/switch <name>` Submit so the persona swap happens in one keypress.
/// What: Populate choices + context, simulate Down + Enter, assert
/// `pending_submit == Some("/switch Izzie")` and `input_buf` is empty.
/// Test: Self-contained.
#[test]
fn inline_choices_switch_context_dispatches_submit() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec!["ctrl".into(), "Izzie".into(), "CTO Assistant".into()];
    a.choice_cursor = 0;
    a.choices_context = Some("switch".into());
    // Down → cursor on "Izzie".
    handle_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(a.choice_cursor, 1);
    // Enter → synthetic `/switch Izzie` queued, input untouched.
    handle_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(a.choices.is_empty(), "choices must clear on selection");
    assert!(
        a.choices_context.is_none(),
        "context must clear on selection"
    );
    assert!(
        a.input_buf.is_empty(),
        "switch context must NOT insert into input buffer"
    );
    assert_eq!(a.pending_submit.as_deref(), Some("/switch Izzie"));
}

/// Why: Inline choices WITHOUT a context (the legacy LLM-offered list
/// path) must keep their original behavior — Enter inserts the
/// selected text into the input buffer for the user to edit/submit.
/// What: choices set, no context; Enter places pick into input_buf
/// and leaves `pending_submit` empty.
/// Test: Self-contained.
#[test]
fn inline_choices_no_context_inserts_into_input() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec!["alpha".into(), "beta".into()];
    a.choice_cursor = 1;
    a.choices_context = None;
    handle_key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(a.input_buf, "beta");
    assert!(a.pending_submit.is_none());
}

/// Why: Slash-command autocomplete must filter SLASH_COMMANDS by the
/// prefix the user has typed so far. Typing `/me` should narrow to
/// `/memories` (and any other `/me*` commands).
/// What: Insert chars one by one; assert `app.choices` contains
/// `/memories` after `/me` is typed and is empty before the leading `/`.
/// Test: Self-contained.
#[test]
fn slash_completions_filters_by_prefix() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    // No `/` yet → no slash autocomplete picker.
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
    );
    assert!(a.choices.is_empty());
    // Reset and type `/`.
    a.input_buf.clear();
    a.cursor_pos = 0;
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
    );
    // Single `/` matches all commands.
    assert!(!a.choices.is_empty(), "expected matches after `/`");
    assert!(a.choices.iter().any(|c| c == "/memories"));
    assert!(a.choices.iter().any(|c| c == "/help"));
    // Narrow with `m` → both `/memories` and `/model` match.
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
    );
    assert!(a.choices.iter().any(|c| c == "/memories"));
    assert!(a.choices.iter().any(|c| c == "/model"));
    assert!(!a.choices.iter().any(|c| c == "/help"));
    // Narrow with `e` → only `/memories` matches.
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
    );
    // `/me` is exact-suppressed only if it's the unique full match — but
    // it's a prefix of `/memories`, so the picker stays with that single
    // entry until the user finishes typing `/memories`.
    assert!(a.choices.iter().any(|c| c == "/memories"));
    assert!(!a.choices.iter().any(|c| c == "/model"));
}

/// Why: Once the user types a space after a slash command, the picker
/// must vanish — they're now typing arguments, not picking a command.
/// What: Type `/help`, picker may appear or be exact-suppressed; type
/// space, assert `choices` is empty.
/// Test: Self-contained.
#[test]
fn slash_completions_clears_on_space() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    for c in "/help".chars() {
        handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    // Type space → choices must clear.
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    );
    assert!(a.choices.is_empty(), "space must dismiss slash picker");
}

/// Why: Tab on an active slash picker must complete the highlighted
/// command into the input buffer (with trailing space) and dismiss the
/// picker, so the user can immediately type arguments.
/// What: Type `/me`, arrow Down to `/memories` (or whichever is at
/// index 1), Tab; assert `input_buf == "<picked> "` and choices clear.
/// Test: Self-contained.
#[test]
fn slash_completions_tab_completes() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    for c in "/me".chars() {
        handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(!a.choices.is_empty(), "expected slash picker after `/me`");
    let pick = a.choices[a.choice_cursor].clone();
    handle_key(&mut a, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(a.input_buf, format!("{pick} "));
    assert_eq!(a.cursor_pos, a.input_buf.len());
    assert!(a.choices.is_empty(), "Tab must dismiss picker");
}

/// Why: When the user has typed an exact full match (e.g. `/help`) the
/// picker should not pop up showing only the same string they already
/// typed — it would just be visual noise.
/// What: Type `/help` (which is the full command); assert choices is
/// empty (suppressed because exact single-match).
/// Test: Self-contained.
#[test]
fn slash_completions_suppressed_on_exact_match() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    for c in "/help".chars() {
        handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    // `/help` is unique — no other command starts with `/help`.
    // Picker should be suppressed.
    assert!(
        a.choices.is_empty(),
        "exact single match must suppress picker, got {:?}",
        a.choices
    );
}

/// Why: Backspacing past the leading `/` must clear the slash picker —
/// once the buffer no longer starts with `/`, autocomplete is irrelevant.
/// What: Type `/me`, backspace 3 times; assert choices empty after the
/// final backspace removes the `/`.
/// Test: Self-contained.
#[test]
fn slash_completions_clears_on_backspace_past_slash() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    for c in "/me".chars() {
        handle_key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(!a.choices.is_empty());
    for _ in 0..3 {
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
    }
    assert_eq!(a.input_buf, "");
    assert!(a.choices.is_empty(), "removing `/` must clear picker");
}

/// Why: Inline-picker context (e.g. `/switch` persona list) must NOT be
/// stomped by the slash-completion helper — those choices have their
/// own lifecycle and Enter-action.
/// What: Set `choices_context = Some("switch")` with persona names;
/// type a `/` char (which would normally trigger slash autocomplete);
/// assert choices and context survive untouched.
/// Test: Self-contained.
#[test]
fn slash_completions_does_not_stomp_context_picker() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.choices = vec!["ctrl".into(), "Izzie".into()];
    a.choices_context = Some("switch".into());
    a.choice_cursor = 0;
    // Send a Char that would normally trigger autocomplete update.
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert_eq!(a.choices, vec!["ctrl".to_string(), "Izzie".to_string()]);
    assert_eq!(a.choices_context.as_deref(), Some("switch"));
}

/// Why: When the picker is open, ALL keys must be intercepted — typing a
/// regular character must NOT leak through to the input editor.
/// What: With picker open, sending KeyCode::Char('x') leaves input_buf
/// empty. With picker closed, the same key inserts into the buffer.
/// Test: Two parallel cases.
#[test]
fn handle_key_modal_gates_input_editor() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.picker = Some(PickerState {
        items: vec!["x".into()],
        selected: 0,
        title: "T".into(),
        kind: PickerKind::Model,
    });
    let r = handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert_eq!(r, None);
    assert!(a.input_buf.is_empty(), "modal must swallow chars");

    // Close picker → same key inserts.
    a.picker = None;
    handle_key(
        &mut a,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert_eq!(a.input_buf, "x");
}

/// Why: The rich statusline must include the `[trusty-agents]` prefix and
/// preserve every piece of the underlying status string so users can read
/// LLM, counts, and the help hint at a glance.
/// What: Build a status_line, render via `build_rich_statusline`, flatten
/// span text, and assert the well-known substrings appear in order.
/// Test: Asserts prefix, ✓ tick, "LLM:", model name, and
/// "All systems go" show up. The Tools/Skills/MCP counts and `/help`
/// hint were intentionally dropped in #293.
#[test]
fn rich_statusline_renders_brackets_and_body() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.status_line = Some("✓ LLM: openrouter:claude-haiku-4-5 · All systems go.".to_string());
    let line = build_rich_statusline(&a);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.starts_with("[trusty-agents] "),
        "missing prefix: {text}"
    );
    assert!(text.contains("✓ "), "missing tick: {text}");
    assert!(text.contains("LLM: "), "missing LLM label: {text}");
    assert!(
        text.contains("openrouter:claude-haiku-4-5"),
        "missing model: {text}"
    );
    assert!(
        !text.contains("anthropic/"),
        "vendor prefix should be stripped: {text}"
    );
    assert!(!text.contains('('), "parens should be removed: {text}");
    assert!(
        text.contains("All systems go."),
        "missing OK marker: {text}"
    );
    // Removed in #293:
    assert!(
        !text.contains("Tools: "),
        "Tools count should be removed: {text}"
    );
    assert!(
        !text.contains("Skills: "),
        "Skills count should be removed: {text}"
    );
    assert!(
        !text.contains("MCP: "),
        "MCP count should be removed: {text}"
    );
    assert!(
        !text.contains("/help"),
        "help hint should be removed: {text}"
    );
}

/// Why: With tokens accumulated, the statusline must inject
/// `↑prompt ↓completion · $cost` before `All systems go.` (#293).
/// What: Set tokens_in/tokens_out, render rich statusline, assert
/// substrings appear.
/// Test: prompt 1500, completion 800 → `↑1.5k ↓0.8k` and a `$` chunk.
#[test]
fn rich_statusline_includes_tokens_and_cost_when_present() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.status_line = Some("✓ LLM: openrouter:claude-haiku-4-5 · All systems go.".to_string());
    a.tokens_in = 1500;
    a.tokens_out = 1200;
    let line = build_rich_statusline(&a);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("↑1.5k ↓1.2k"), "missing token chunk: {text}");
    assert!(text.contains('$'), "missing cost chunk: {text}");
}

/// Why: When no tokens have been used yet, the token + cost segments
/// must be omitted (don't show `↑0 ↓0 · $0.0000`) — #293.
/// What: Fresh app with status_line set but tokens at 0, render and
/// confirm the arrow/dollar glyphs are absent.
/// Test: assert! NOT contains.
#[test]
fn rich_statusline_omits_tokens_when_zero() {
    let mut a = ReplApp::new("ctrl".into(), "u".into());
    a.status_line = Some("✓ LLM: openrouter:claude-haiku-4-5 · All systems go.".to_string());
    let line = build_rich_statusline(&a);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!text.contains('↑'), "token arrow should be absent: {text}");
    assert!(!text.contains('$'), "cost glyph should be absent: {text}");
}

/// Why: `format_tokens` is the shared compactor for both the spinner and
/// the statusline; small numbers stay raw, ≥1000 collapses to `Nk` form.
/// What: Spot-check key boundaries and fractional rounding.
/// Test: 0, 999, 1000, 1234, 2000, 12345.
#[test]
fn format_tokens_compact() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(999), "999");
    assert_eq!(format_tokens(1000), "1k");
    assert_eq!(format_tokens(1234), "1.2k");
    assert_eq!(format_tokens(2000), "2k");
    assert_eq!(format_tokens(12345), "12.3k");
}

/// Why: `format_elapsed` powers the Claude Code-style `(2m 18s · …)`
/// spinner timer; verify the three buckets.
/// What: 5s → "5s", 78s → "1m 18s", 3700s → "1h 1m".
/// Test: deterministic mapping.
#[test]
fn format_elapsed_buckets() {
    assert_eq!(format_elapsed(0), "0s");
    assert_eq!(format_elapsed(5), "5s");
    assert_eq!(format_elapsed(59), "59s");
    assert_eq!(format_elapsed(60), "1m 0s");
    assert_eq!(format_elapsed(78), "1m 18s");
    assert_eq!(format_elapsed(3700), "1h 1m");
}

/// Why: `status_word_for` cycles through thinking/working/processing on
/// elapsed-time buckets — verify boundaries.
/// What: 0/9/10/29/30s thresholds.
/// Test: deterministic.
#[test]
fn status_word_buckets() {
    assert_eq!(status_word_for(0), "thinking");
    assert_eq!(status_word_for(9), "thinking");
    assert_eq!(status_word_for(10), "working");
    assert_eq!(status_word_for(29), "working");
    assert_eq!(status_word_for(30), "processing");
    assert_eq!(status_word_for(600), "processing");
}

/// Why: `format_token_chunk` is the statusline arrow form — must use
/// the compact `k` form for both directions.
/// What: ↑1.2k ↓0.8k for 1234/800.
/// Test: deterministic.
#[test]
fn format_token_chunk_compacts_thousands() {
    assert_eq!(format_token_chunk(1234, 800), "↑1.2k ↓800");
    assert_eq!(format_token_chunk(0, 0), "↑0 ↓0");
}

/// Why: Cost format precision flips at $0.01 — 4 decimals below, 3 at/above.
/// What: Tiny costs render with 4 decimals, larger ones with 3.
/// Test: deterministic boundary check.
#[test]
fn format_cost_chunk_thresholds() {
    // 1000 prompt + 1000 completion @ haiku rates =
    //   1000 * 0.00000025 + 1000 * 0.00000125 = 0.0015
    let s = format_cost_chunk(1000, 1000);
    assert_eq!(s, "$0.0015");
    // 100k prompt + 100k completion = 0.025 + 0.125 = 0.150 → 3 decimals.
    let s = format_cost_chunk(100_000, 100_000);
    assert_eq!(s, "$0.150");
}

/// Why: Activity row 1 must include elapsed time and the cycling status
/// word, with `✻` glyph and `Processing…` label (Claude Code style).
/// What: Build with elapsed=5 and zero tokens; assert glyph + word.
/// Test: substring presence.
#[test]
fn activity_row1_includes_elapsed_and_status() {
    let a = ReplApp::new("ctrl".into(), "u".into());
    let line = build_activity_row1(&a, 5, 120);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    // Spinner is animated — assert the leading glyph is one of the
    // braille frames rather than a hardcoded character.
    assert!(
        SPINNER_FRAMES.iter().any(|f| text.starts_with(f)),
        "spinner glyph not in animation set: {text}"
    );
    assert!(text.contains("Processing…"), "missing label: {text}");
    assert!(text.contains("5s"), "missing elapsed: {text}");
    assert!(text.contains("thinking"), "missing status word: {text}");
    // Token segment should be omitted with zero tokens.
    assert!(!text.contains('↓'), "token segment leaked at zero: {text}");
}
