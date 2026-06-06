//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Detect a multiple-choice list in an LLM response and extract the items.
///
/// Why: When the model asks the user to pick from a list (numbered "1." / "2."
/// or bulleted "- " / "• "), surfacing those choices as an interactive picker
/// below the input row is much faster than retyping. We only trigger when at
/// least 2 list items are present so prose with a single bullet doesn't false-
/// positive into picker mode.
/// What: Returns `Some(items)` if the response contains either:
///  - 2+ lines starting with `N.` (numbered list)
///  - 2+ lines starting with `- ` or `• ` (bulleted list)
/// The body of each list item (after the marker) is captured, trimmed, and
/// returned. Otherwise returns `None`.
/// Test: `detect_choices_*` unit tests below.
pub fn detect_choices(text: &str) -> Option<Vec<String>> {
    let mut numbered: Vec<String> = Vec::new();
    let mut bulleted: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        // Numbered: digits followed by `.` or `)` then space.
        if let Some(rest) = strip_numbered_marker(line) {
            let body = rest.trim();
            if !body.is_empty() {
                numbered.push(body.to_string());
            }
            continue;
        }
        // Bulleted.
        if let Some(rest) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("• "))
            .or_else(|| line.strip_prefix("* "))
        {
            let body = rest.trim();
            if !body.is_empty() {
                bulleted.push(body.to_string());
            }
        }
    }
    if numbered.len() >= 2 {
        return Some(numbered);
    }
    if bulleted.len() >= 2 {
        return Some(bulleted);
    }
    None
}

/// Strip a numbered-list marker (`1.`, `12)`, etc.) from the start of a line.
///
/// Why: Pulled out so `detect_choices` can stay readable and so the marker
/// parsing is unit-testable in isolation.
/// What: If the line starts with one or more digits followed by `.` or `)` and
/// then a space, returns the remainder; otherwise None.
/// Test: covered by `detect_choices_*` tests.
pub(crate) fn strip_numbered_marker(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    if i >= bytes.len() {
        return None;
    }
    let punct = bytes[i];
    if punct != b'.' && punct != b')' {
        return None;
    }
    let after_punct = i + 1;
    if after_punct >= bytes.len() {
        return None;
    }
    let n = line[after_punct..].chars().next()?;
    if !n.is_whitespace() {
        return None;
    }
    Some(&line[after_punct + n.len_utf8()..])
}

/// Render the inline multiple-choice picker (just below the input row).
///
/// Why: A non-modal picker right beneath the input keeps the user's eyes near
/// where they're typing — no popup, no chat scroll disruption. The picker
/// shrinks the chat pane by its own height so nothing visually jumps when it
/// appears or disappears.
/// What: Borderless flat list of choices — no Block, no title. Selected row
/// gets a `▶ ` cyan/dim prefix and bold text; non-selected rows get `  `
/// (two-space) prefix and dim text. When choices exceed the visible area,
/// a sliding window keeps the cursor visible.
/// Test: Visual via tmux REPL; state mutations covered by `inline_choice_*`.
pub(crate) fn draw_inline_choice_picker(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    if app.choices.is_empty() || area.height < 1 {
        return;
    }

    // Compute sliding window so cursor stays visible. Window size is the
    // smaller of the rendered area height and the total number of choices.
    let total = app.choices.len();
    let visible = (area.height as usize).min(total);
    if visible == 0 {
        return;
    }
    let start = if total <= visible {
        0
    } else if app.choice_cursor >= visible / 2 {
        let raw = app.choice_cursor.saturating_sub(visible / 2);
        raw.min(total - visible)
    } else {
        0
    };
    let end = (start + visible).min(total);

    let lines: Vec<Line> = app.choices[start..end]
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let abs = start + i;
            let is_sel = abs == app.choice_cursor;
            let indicator = if is_sel { "▶ " } else { "  " };
            let indicator_span = Span::styled(
                indicator,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            );
            let text_span = if is_sel {
                Span::styled(item.clone(), Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(item.clone(), Style::default().add_modifier(Modifier::DIM))
            };
            Line::from(vec![indicator_span, text_span])
        })
        .collect();

    let para = Paragraph::new(lines);
    f.render_widget(para, area);
}

/// Render the picker overlay if one is open.
///
/// Why: `/model` and `/provider` (no arg) open a modal list so the user can
/// pick interactively instead of typing a model id from memory. Drawn last
/// so it sits on top of the chat / input bar.
/// What: Centered popup (~50% × 60%), `Clear` widget under the border so the
/// chat behind it is masked, items rendered as a `List` with the selected
/// row highlighted (cyan + bold + ● marker), footer hint at the bottom.
/// Test: Manual via tmux REPL — open `/model` and `/provider`, navigate, Esc.
pub(crate) fn draw_picker(f: &mut ratatui::Frame, app: &ReplApp) {
    let Some(picker) = &app.picker else { return };

    let area = centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = picker
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let is_sel = i == picker.selected;
            let content = if is_sel {
                format!("● {}", item)
            } else {
                format!("  {}", item)
            };
            let style = if is_sel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(content).style(style)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" {} ", picker.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    // Reserve the bottom row of the popup for the footer hint.
    let inner = block.inner(area);
    f.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default();
    list_state.select(Some(picker.selected));
    f.render_stateful_widget(list, split[0], &mut list_state);

    let hint = Paragraph::new(Line::from(Span::styled(
        "↑↓ navigate  Enter select  Esc cancel",
        Style::default().add_modifier(Modifier::DIM),
    )))
    .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(hint, split[1]);
}
