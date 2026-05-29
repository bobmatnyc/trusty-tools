//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Top-level draw: banner (optional) + chat (fills) + activity (3 rows, busy)
/// + streaming preview (2 rows, busy) + input separator + input + bottom
/// separator + statusline.
///
/// Why: When the LLM is working we want a dedicated, persistent activity
/// strip near the input — spinner + elapsed time + active model + the latest
/// thinking step — and a queued-response preview right above the prompt so
/// the user sees the assistant taking shape without scrolling. When idle
/// those rows collapse to zero so the chat fills the screen.
/// What: Layout constraints are computed dynamically from `app.busy()`
/// (true ⇔ thinking || busy_since.is_some()). The top separator that used
/// to sit between chat and input is removed when busy — the activity area
/// itself provides visual separation. The bottom separator (above the
/// statusline) stays put.
/// Test: Visual via `scripts/tmux-repl-test.sh`; geometry exercised via
/// the `draw_*` helpers + `repl_app_busy_*` unit tests on state.
pub fn draw(f: &mut ratatui::Frame, app: &ReplApp) {
    let busy = app.thinking || app.busy_since.is_some();

    // Inline choice picker height — computed early so chat sizing math (below)
    // can subtract it from available height. Same value used when pushing
    // constraint further down.
    let picker_height: u16 = if !app.choices.is_empty() {
        app.choices.len().min(8) as u16
    } else {
        0
    };

    // Build the constraint vector with semantic markers so destructuring is
    // explicit. The order is always:
    //   chat [activity preview] sep_above_input input sep_below_input statusline
    //
    // Note: the banner is no longer a separate layout chunk — it is prepended
    // to the chat line buffer in `draw_chat()` when `app.show_banner` is true,
    // so it scrolls naturally with chat history.
    let mut constraints: Vec<Constraint> = Vec::with_capacity(8);
    // Chat constraint: when both the banner and chat are absent, collapse to
    // Length(0) so the empty pane doesn't expand to fill the terminal. Once
    // either banner or chat content is present, size to actual content so the
    // input row floats up directly beneath the last message (issue #337). The
    // trailing Min(1) at the bottom of the layout absorbs remaining space.
    if app.chat.is_empty() && !app.show_banner {
        constraints.push(Constraint::Length(0)); // chat (empty — no forced expansion)
    } else if app.chat.is_empty() && app.show_banner {
        // Startup splash: size the chat pane to exactly the banner's height so
        // the bottom border sits flush against the input separator, instead of
        // letting an expanding constraint inflate the pane and leave a sea of
        // blank rows below the banner content. The trailing Min(1) spacer
        // below the statusline absorbs any leftover vertical space.
        let banner_h = banner_lines(app, f.area().width as usize).len() as u16;
        constraints.push(Constraint::Length(banner_h));
    } else {
        // Issue #337: size chat pane to actual content height (capped at the
        // available space). Previously used `Constraint::Min(3)` which expands
        // to fill the screen, pinning the input box to the bottom even with a
        // 5-line conversation. Length(content_h) lets the trailing `Min(1)`
        // spacer absorb leftover rows so input/statusline sit flush below the
        // content. When content >= available_h, behavior matches the old
        // Min(3) path because chat consumes all available rows.
        let content_h = chat_line_count(app, f.area().width as usize).max(1);
        // Reserved rows below chat: top_sep(1) + input(1) + bot_sep(1)
        // + activity(3 if busy) + picker_height + statusline(1)
        // + bottom spacer minimum(1).
        let reserved: u16 = 1 // top_sep
            + 1 // input
            + 1 // bot_sep
            + if busy { 3 } else { 0 } // activity
            + picker_height // inline picker
            + 1 // statusline
            + 1; // bottom spacer minimum
        let available_h = f.area().height.saturating_sub(reserved) as usize;
        let chat_h = content_h.min(available_h).max(1);
        constraints.push(Constraint::Length(chat_h as u16));
    }
    if busy {
        constraints.push(Constraint::Length(3)); // activity (busy only)
    }
    // Single separator above input. The dedicated queued-hint row was folded
    // into the empty input row as a dim italic placeholder (Claude Code style).
    constraints.push(Constraint::Length(1)); // separator above input
    constraints.push(Constraint::Length(1)); // input
    constraints.push(Constraint::Length(1)); // bottom separator
    // Inline choice picker (when active) sits BETWEEN the bottom input
    // separator and the statusline so it visually anchors below the input
    // box rather than crowding the input row itself. Height = min(N, 8) —
    // borderless flat list, capped so a long list doesn't crowd out the
    // chat area. (`picker_height` computed above for chat sizing.)
    if picker_height > 0 {
        constraints.push(Constraint::Length(picker_height));
    }
    constraints.push(Constraint::Length(1)); // statusline
    // Bug 2: a single blank terminal row below the statusline at all times
    // gives the bottom bar breathing room from the terminal's bottom edge.
    // Using Min(1) here (rather than Length(1)) doubles as the leftover-
    // absorber when chat is collapsed to Length(0) on startup, so the input
    // doesn't get pushed to the bottom by ratatui's space distribution.
    constraints.push(Constraint::Min(1)); // blank spacer below statusline

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(f.area());

    // Walk the chunks in the same order they were pushed.
    let mut idx = 0usize;
    let chat_area = chunks[idx];
    idx += 1;
    let activity_area = if busy {
        let a = chunks[idx];
        idx += 1;
        Some(a)
    } else {
        None
    };
    let top_sep_area = chunks[idx];
    idx += 1;
    let input_area = chunks[idx];
    idx += 1;
    let bot_sep_area = chunks[idx];
    idx += 1;
    let inline_picker_area = if picker_height > 0 {
        let a = chunks[idx];
        idx += 1;
        Some(a)
    } else {
        None
    };
    let status_area = chunks[idx];
    idx += 1;
    // Bug 2: trailing blank-row spacer below the statusline. Rendering an
    // explicit empty Paragraph here is defensive — the row is reserved by the
    // layout regardless, but rendering ensures alt-screen state is clean even
    // if some prior frame left artifacts.
    let bottom_spacer_area = chunks[idx];

    draw_chat(f, app, chat_area);
    if let Some(a) = activity_area {
        draw_activity(f, app, a);
    }
    draw_separator(f, top_sep_area);
    draw_input(f, app, input_area);
    draw_separator(f, bot_sep_area);
    if let Some(a) = inline_picker_area {
        draw_inline_choice_picker(f, app, a);
    }
    draw_statusline(f, app, status_area);
    // Bug 2: render the trailing blank row (no border, no text).
    f.render_widget(Paragraph::new(""), bottom_spacer_area);
    // Picker overlay renders LAST so it sits on top of every other widget.
    draw_picker(f, app);
}

/// Render the activity panel (spinner + elapsed time + model + latest step).
///
/// Why: Persistent feedback that the LLM call is in flight, distinct from
/// the inline `[thinking...]` label on the input row. Three rows so we can
/// surface the model id and the most recent thinking step alongside the
/// spinner without crowding the input.
/// What: Row 1 = spinner glyph + "processing..." (dim) + right-aligned
/// elapsed `Xs`. Row 2 = `↳ model: <model_name>` (dim). Row 3 = latest
/// thinking step (dim italic), truncated to width.
/// Test: Geometry via `scripts/tmux-repl-test.sh`; spinner cycle is
/// time-based and deterministic given the same `Instant`.
pub(crate) fn draw_activity(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let elapsed_secs = app.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);

    let row1 = build_activity_row1(app, elapsed_secs, area.width as usize);

    // Row 2: model id (kept). Strip the vendor prefix (`anthropic/`, `openai/`,
    // …) so users see the bare model name. The full id is still present in
    // logs and the statusline-source string for debugging.
    let model_text = format!("  ↳ {}", strip_vendor_prefix(&app.model_name));
    let row2 = Line::from(Span::styled(
        truncate_to(model_text, area.width as usize),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::ITALIC),
    ));

    // Row 3: latest thinking step or blank. Dedup against row 1's cycling
    // status word ("thinking" / "working" / "processing") — when the only
    // thinking line is identical (case-insensitive, modulo trailing
    // ellipses/whitespace) to the spinner word, leave row 3 blank rather
    // than echoing it twice.
    let raw_step = app.thinking_lines.last().cloned().unwrap_or_default();
    let step_text = if is_redundant_thinking_step(&raw_step) {
        String::new()
    } else {
        raw_step
    };
    let row3 = Line::from(Span::styled(
        truncate_to(format!("  {}", step_text), area.width as usize),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM | Modifier::ITALIC),
    ));

    let p = Paragraph::new(vec![row1, row2, row3]);
    f.render_widget(p, area);
}
