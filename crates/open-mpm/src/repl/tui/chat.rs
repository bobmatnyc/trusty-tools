//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Build the rendered `Vec<Line>` for the chat pane.
///
/// Why: Extracted from `draw_chat` so layout code in `draw()` can compute the
/// chat content height *before* layout splitting, enabling input to follow
/// content (issue #337) instead of being pinned to the screen bottom by
/// `Constraint::Min(3)`.
/// What: Replicates the banner + chat entry line-building logic, returns the
/// post-collapse, pre-pad `Vec<Line>`. Width-dependent rendering (banner
/// wrapping, markdown table column sizing) uses the supplied `terminal_width`.
/// Test: Compare `build_chat_lines(app, w).len()` against the line count
/// observed inside `draw_chat` for representative `app.chat` fixtures.
pub(crate) fn build_chat_lines(app: &ReplApp, terminal_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Prepend the welcome banner as part of the chat scroll buffer. As new
    // messages arrive, the banner scrolls upward like terminal command history
    // and eventually disappears off the top — no explicit hide step needed.
    if app.show_banner {
        lines.extend(banner_lines(app, terminal_width));
        lines.push(Line::from("")); // gap between banner and first chat entry
    }

    let chat_len = app.chat.len();
    for (idx, entry) in app.chat.iter().enumerate() {
        match entry.role {
            ChatRole::User => {
                let mut iter = entry.text.lines();
                if let Some(first) = iter.next() {
                    lines.push(Line::from(vec![
                        Span::styled("❯", Style::default().fg(Color::Green)),
                        Span::raw(" "),
                        Span::raw(first.to_string()),
                    ]));
                }
                for cont in iter {
                    lines.push(Line::from(format!("  {}", cont)));
                }
                // Note: previously rendered `⟳ thinking…` lines beneath the
                // latest user prompt while the LLM was busy. Removed — the
                // activity strip (`draw_activity`) is now the sole live
                // thinking indicator. The `thinking_lines` event flow is
                // unaffected and still feeds row 3 of the activity strip.
            }
            ChatRole::Assistant | ChatRole::Error => {
                let body_color = if entry.role == ChatRole::Error {
                    Some(Color::Red)
                } else {
                    None
                };
                // Collect lines into a Vec for index-based table-block
                // lookahead. Walking with an iterator alone makes it awkward
                // to peek the separator row that distinguishes a table from
                // an arbitrary `|`-prefixed line.
                let body_lines: Vec<&str> = entry.text.lines().collect();
                let mut i = 0usize;
                let mut emitted_first = false;
                while i < body_lines.len() {
                    // Fenced code-block detection (#321). A line of the form
                    // ` ```<lang> ` opens a block; the next ` ``` ` closes it.
                    // Executable shell blocks (bash/sh/zsh/fish) get a bright
                    // green `▶ <lang>` header; other languages get a neutral
                    // `⬡ <lang>` header in light blue. Body lines render as
                    // dim gray under the standard 3-space indent. The closing
                    // fence renders as a thin dim separator.
                    if let Some(lang) = code_fence_lang(body_lines[i]) {
                        let is_shell = is_executable_shell_lang(&lang);
                        // Emit the agent leader if this is the first body
                        // element — keeps the `⏺ <name> · ` prefix consistent
                        // with table/prose paths above.
                        if !emitted_first {
                            let mut spans = vec![
                                Span::styled("⏺", Style::default().fg(Color::Indexed(208))),
                                Span::raw(" "),
                            ];
                            if body_color.is_none() {
                                let label_color = match app.agent_scope {
                                    AgentScope::User => Color::Cyan,
                                    AgentScope::Project => Color::Yellow,
                                };
                                spans.push(Span::styled(
                                    app.project_name.clone(),
                                    Style::default()
                                        .fg(label_color)
                                        .add_modifier(Modifier::BOLD),
                                ));
                                spans.push(Span::styled(
                                    " · ",
                                    Style::default().add_modifier(Modifier::DIM),
                                ));
                            }
                            lines.push(Line::from(spans));
                            emitted_first = true;
                        }
                        // Header line.
                        let header_label = if lang.is_empty() {
                            "code".to_string()
                        } else {
                            lang.clone()
                        };
                        if is_shell {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    format!("▶ {}", header_label),
                                    Style::default()
                                        .fg(Color::LightGreen)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        } else {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    format!("⬡ {}", header_label),
                                    Style::default().fg(Color::Indexed(75)),
                                ),
                            ]));
                        }
                        // Body lines until the closing fence (or EOF).
                        let mut j = i + 1;
                        let body_style = Style::default().fg(Color::Indexed(244));
                        while j < body_lines.len() {
                            if code_fence_lang(body_lines[j]).is_some() {
                                break;
                            }
                            lines.push(Line::from(Span::styled(
                                format!("   {}", body_lines[j]),
                                body_style,
                            )));
                            j += 1;
                        }
                        // Closing fence — emit a thin dim separator.
                        // #324: Verify j landed on a real *closer* (lang == ""),
                        // not a nested opener. If body_lines[j] is itself a
                        // fence opener (e.g. docs example showing ```bash`
                        // inside another block), treating it as a closer would
                        // advance `i` past a real fence start and corrupt all
                        // subsequent rendering. Treat malformed/nested cases
                        // as unclosed: don't consume the line at j.
                        let j_is_closer = j < body_lines.len()
                            && code_fence_lang(body_lines[j]).is_some_and(|lang| lang.is_empty());
                        if j_is_closer {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    "─".repeat(20),
                                    Style::default().fg(Color::Indexed(240)),
                                ),
                            ]));
                            i = j + 1;
                        } else {
                            // Nested opener or EOF — block is unclosed; no
                            // closing separator. Don't consume body_lines[j]
                            // (it's either EOF or a new fence opener that the
                            // outer loop must process).
                            i = j;
                        }
                        continue;
                    }
                    // Try to detect a markdown table starting at i: header
                    // row + separator row + zero or more data rows.
                    let is_table_start = i + 1 < body_lines.len()
                        && is_md_table_row(body_lines[i])
                        && is_md_table_separator(body_lines[i + 1]);

                    if is_table_start {
                        let header = parse_md_table_cells(body_lines[i]);
                        let mut j = i + 2;
                        let mut body_rows: Vec<Vec<String>> = Vec::new();
                        while j < body_lines.len() && is_md_table_row(body_lines[j]) {
                            body_rows.push(parse_md_table_cells(body_lines[j]));
                            j += 1;
                        }
                        // Indent under the `⏺ ` glyph (3 spaces) — matches
                        // how non-table continuation lines are indented so
                        // the table aligns with surrounding body text.
                        // Both branches currently render the same three-space
                        // continuation indent; kept as a single constant for
                        // clarity (and to make future per-branch styling a
                        // one-line tweak).
                        let indent = "   ";
                        // Available width: the chat pane width. Subtract a
                        // tiny safety margin for ratatui's wrap behavior.
                        let avail = terminal_width.saturating_sub(1);
                        let table_lines =
                            render_markdown_table(&header, &body_rows, avail, indent, body_color);
                        // If this is the very first body line, we still need
                        // to emit the leader (`⏺ <name> · `) before the table.
                        // Produce a minimal leader line, then push table rows.
                        if !emitted_first {
                            let mut spans = vec![
                                Span::styled("⏺", Style::default().fg(Color::Indexed(208))),
                                Span::raw(" "),
                            ];
                            if body_color.is_none() {
                                let label_color = match app.agent_scope {
                                    AgentScope::User => Color::Cyan,
                                    AgentScope::Project => Color::Yellow,
                                };
                                spans.push(Span::styled(
                                    app.project_name.clone(),
                                    Style::default()
                                        .fg(label_color)
                                        .add_modifier(Modifier::BOLD),
                                ));
                                spans.push(Span::styled(
                                    " · ",
                                    Style::default().add_modifier(Modifier::DIM),
                                ));
                            }
                            // Empty content span — table follows on next line.
                            lines.push(Line::from(spans));
                            emitted_first = true;
                        }
                        for tl in table_lines {
                            lines.push(tl);
                        }
                        i = j;
                        continue;
                    }

                    // Non-table line: emit as before.
                    let raw = body_lines[i];
                    if !emitted_first {
                        let mut spans = vec![
                            Span::styled("⏺", Style::default().fg(Color::Indexed(208))),
                            Span::raw(" "),
                        ];
                        if body_color.is_none() {
                            let label_color = match app.agent_scope {
                                AgentScope::User => Color::Cyan,
                                AgentScope::Project => Color::Yellow,
                            };
                            spans.push(Span::styled(
                                app.project_name.clone(),
                                Style::default()
                                    .fg(label_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            spans.push(Span::styled(
                                " · ",
                                Style::default().add_modifier(Modifier::DIM),
                            ));
                        }
                        spans.push(match body_color {
                            Some(c) => Span::styled(raw.to_string(), Style::default().fg(c)),
                            None => Span::raw(raw.to_string()),
                        });
                        lines.push(Line::from(spans));
                        emitted_first = true;
                    } else {
                        // Blank source lines render as a TRULY empty Line so
                        // the post-pass `collapse_blank_lines` can fold them
                        // against siblings without the leading "   " indent
                        // throwing off its whitespace-only detection in
                        // adjacent renders. Non-blank continuations keep the
                        // 3-space indent so they line up under the `⏺ ` glyph.
                        let line = if raw.trim().is_empty() {
                            Line::from("")
                        } else {
                            match body_color {
                                Some(c) => Line::from(Span::styled(
                                    format!("   {}", raw),
                                    Style::default().fg(c),
                                )),
                                None => Line::from(Span::raw(format!("   {}", raw))),
                            }
                        };
                        lines.push(line);
                    }
                    i += 1;
                }
            }
            ChatRole::Status => {
                lines.push(Line::from(vec![
                    Span::styled("[open-mpm] ", Style::default().fg(Color::Green)),
                    Span::raw(entry.text.clone()),
                ]));
            }
        }
        // Inter-message separator: insert exactly one blank line between
        // every pair of distinct chat entries (regardless of role) so
        // sections never run together. Skip after the very last entry —
        // the trailing blank spacer below + bottom-pinned layout already
        // provide the gap above the input separator. Any doubling that
        // results from a message ending in its own blank is normalized
        // by the post-pass `collapse_blank_lines`.
        let is_last = idx + 1 == chat_len;
        if !is_last {
            lines.push(Line::from(""));
        }
    }

    // Collapse runs of consecutive blank lines (≥2 → 1) before pinning so
    // markdown-style `\n\n` paragraph breaks don't render as wasted vertical
    // space in the terminal. Operates after wrapping decisions but before
    // pad/scroll math so the geometry math sees the post-collapse line count.
    let mut lines = collapse_blank_lines(lines);

    // Always reserve one trailing blank line as breathing room between the
    // last chat message and the input/separator below. When chat is empty we
    // still skip this — there's nothing to give breathing room from, and
    // adding a stray blank would push the (suppressed) banner geometry off.
    if !app.chat.is_empty() {
        lines.push(Line::from(""));
    }

    lines
}

/// Count chat content lines for layout sizing.
///
/// Why: `draw()` needs to know the rendered content height *before* the
/// vertical layout split so the chat pane can use `Constraint::Length(h)`
/// instead of `Min(3)` (issue #337) — `Min` would expand to fill all
/// available space, pinning the input box to the screen bottom even when
/// the conversation is short.
/// What: Returns the post-collapse, pre-pad line count from `build_chat_lines`.
/// Test: For an empty chat with `show_banner=false`, returns 0; for a
/// non-empty chat, returns at least the chat entry count.
pub fn chat_line_count(app: &ReplApp, terminal_width: usize) -> usize {
    build_chat_lines(app, terminal_width).len()
}

pub(crate) fn draw_chat(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    // The startup `status_line` is now rendered in the bottom statusline row
    // (see `draw_statusline` / `build_rich_statusline`) instead of duplicated
    // here at the top of chat.
    let lines = build_chat_lines(app, area.width as usize);

    // Bottom-pinned chat: when chat has content and it fits in the visible
    // area, prepend blank padding lines so the content sits flush at the
    // BOTTOM of the chat pane (chat-app feel — like iMessage / Slack). On
    // startup with no messages we skip padding entirely so the pane stays
    // visually empty. When content overflows the visible area we don't pad
    // — the existing `max_offset` logic auto-scrolls so the newest line
    // stays in view (the user can PageUp to scroll back).
    let visible = area.height as usize;
    let total = lines.len();

    let final_lines = if !app.chat.is_empty() && total < visible {
        let pad = visible - total;
        let mut padded = Vec::with_capacity(visible);
        for _ in 0..pad {
            padded.push(Line::from(""));
        }
        padded.extend(lines);
        padded
    } else {
        lines
    };

    // When content fits (final_total <= visible), max_offset is 0 → no scroll.
    // When content overflows, max_offset pushes the newest content to the
    // bottom. `app.scroll_offset` lets the user scroll up from pinned.
    let final_total = final_lines.len();
    let max_offset = final_total.saturating_sub(visible);
    // #329: Publish the rendered max so `ReplApp::scroll` can clamp future
    // wheel/PageUp deltas. Shared via Arc<AtomicUsize> so this snapshot's
    // write is visible to the authoritative ReplApp behind the runtime mutex.
    app.last_max_scroll
        .store(max_offset, std::sync::atomic::Ordering::Relaxed);
    let effective_offset = max_offset.saturating_sub(app.scroll_offset);

    let paragraph = Paragraph::new(final_lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_offset as u16, 0));
    f.render_widget(paragraph, area);
}

pub(crate) fn draw_input(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    // Borderless input row — Claude Code-style. The prompt label `<name>>`
    // is sufficient demarcation; the surrounding box wasted a row of vertical
    // space and clashed with the new statusline row below.
    let inner = area;

    // Compose: `<label>> <input>                    [thinking...]`
    let prompt = format!("{}> ", app.project_name);
    let prompt_width = prompt.chars().count();
    let total_width = inner.width as usize;

    let thinking_label = "[thinking...]";
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(prompt.clone()));
    // Empty input: render dim italic placeholder (Claude Code style).
    // - Busy + empty: show "↑ to cancel"
    // - Idle + empty: show the discoverability hint
    // - Non-empty: render the buffer normally
    if app.input_buf.is_empty() {
        let placeholder = if app.thinking || app.busy_since.is_some() {
            "↑ to cancel"
        } else {
            "Ask ctrl anything, or /connect <path> for project work"
        };
        spans.push(Span::styled(
            placeholder.to_string(),
            Style::default()
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        spans.push(Span::raw(app.input_buf.clone()));
    }

    // Right-side decoration: prefer `[thinking...]` while busy; otherwise show
    // token counters when they are non-zero. Tokens render in dim style as
    // `↑{in} ↓{out}` to mirror the StatusBar format.
    let token_label: Option<String> = match (app.tokens_in, app.tokens_out) {
        (0, 0) => None,
        (0, out) => Some(format!("↓{}", out)),
        (inp, 0) => Some(format!("↑{}", inp)),
        (inp, out) => Some(format!("↑{} ↓{}", inp, out)),
    };

    if app.thinking {
        let used = prompt_width + app.input_buf.chars().count();
        let label_w = thinking_label.chars().count();
        if total_width > used + label_w + 1 {
            let pad = total_width - used - label_w;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                thinking_label,
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    } else if let Some(ref tok) = token_label {
        let used = prompt_width + app.input_buf.chars().count();
        let label_w = tok.chars().count();
        if total_width > used + label_w + 1 {
            let pad = total_width - used - label_w;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                tok.clone(),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }

    let line = Line::from(spans);
    let p = Paragraph::new(line);
    f.render_widget(p, inner);

    // Position the terminal cursor visually at the input position.
    let cursor_col =
        inner.x + (prompt_width as u16) + (app.input_buf[..app.cursor_pos].chars().count() as u16);
    let cursor_row = inner.y;
    f.set_cursor_position((cursor_col, cursor_row));
}
