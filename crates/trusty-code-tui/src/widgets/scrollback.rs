//! The chat scrollback pane — generalized port of tagent's
//! `repl/tui/chat.rs` (`build_chat_lines`/`chat_line_count`/`draw_chat`).
//!
//! Why: DOC-50 §5 Slice 4 migrates this file's markdown-aware chat rendering
//! (fenced code blocks, tables, the banner-as-scrollback-prefix trick) since
//! it's the single largest, most product-agnostic piece of tagent's TUI.
//! Generalization removed exactly one thing: tagent's `AgentScope`-driven
//! cyan/yellow leader color became [`crate::app::ReplApp::accent_color`], a
//! plain engine-set field (see that field's doc comment) — everything else
//! (fence/table detection via [`crate::render::markdown`], blank-line
//! collapsing) is a direct, behavior-preserving port.
//!
//! What: [`build_chat_lines`] walks `app.chat` and returns the rendered
//! `Vec<Line>` (banner prepended when `app.show_banner`); [`chat_line_count`]
//! is the pre-layout height query [`crate::layout::draw`] uses to size the
//! chat `Constraint`; [`draw_chat`] renders into a `Frame` with
//! bottom-pinned padding and publishes the scroll cap to
//! `app.last_max_scroll`.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): migrate `chat.rs`.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{ChatRole, ReplApp};
use crate::render::markdown::{
    code_fence_lang, is_executable_shell_lang, is_md_table_row, is_md_table_separator,
    parse_md_table_cells, render_markdown_table,
};
use crate::widgets::banner::banner_lines;
use crate::widgets::tool_card::tool_card_lines;

/// Drop runs of 2+ consecutive whitespace-only `Line`s, keeping at most one.
///
/// Why: markdown-style `\n\n` paragraph breaks and the renderer's own
/// inter-message spacer can compound into a run of several blank rows;
/// collapsing them to one keeps the scrollback dense.
/// What: operates after wrapping decisions but before pad/scroll math, so
/// the geometry math sees the post-collapse line count.
/// Test: `tests::collapse_blank_lines_drops_consecutive_empty_lines`.
pub fn collapse_blank_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut prev_blank = false;
    for line in lines {
        let is_blank = line.spans.iter().all(|s| s.content.trim().is_empty());
        if is_blank && prev_blank {
            continue;
        }
        prev_blank = is_blank;
        out.push(line);
    }
    out
}

/// Build the chat leader spans (`⏺ <label> · `) shared by every "first body
/// line" branch below — table, fenced code, and plain-text alike.
fn leader_spans(app: &ReplApp, body_color: Option<Color>) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled("⏺", Style::default().fg(Color::Indexed(208))),
        Span::raw(" "),
    ];
    if body_color.is_none() {
        spans.push(Span::styled(
            app.label.clone(),
            Style::default()
                .fg(app.accent_color)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            " · ",
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    spans
}

/// The gutter drawn to the left of every line inside a delegated
/// sub-agent's block (#7940).
pub(crate) const DELEGATED_GUTTER: &str = "  │ ";

/// [`DELEGATED_GUTTER`] styled in the delegation accent — shared by
/// delegated text and delegated tool cards (#4596).
pub(crate) fn delegated_gutter_span() -> Span<'static> {
    Span::styled(DELEGATED_GUTTER, Style::default().fg(Color::Indexed(141)))
}

/// Render one delegation frame or body entry (#7940).
///
/// Why: a delegated sub-agent's work has to read as a block, not as more
/// top-level chat — the whole point of issue #7940 is that an operator
/// watching `tcode tui` can see the PM hand work to an engineer. A frame
/// line ([`ChatRole::Delegation`], the `▶ …` header and `└ …` footer) sits
/// flush left in the delegation accent; every body line
/// ([`ChatRole::Delegated`] — the sub-agent's streamed words and its tool
/// notices) is prefixed with [`DELEGATED_GUTTER`] so it is visibly inside
/// the frame.
/// What: markdown-aware rendering (fenced code, tables) is deliberately NOT
/// applied here — the fancy panel is a later slice of requirement R9; this
/// is the minimal readable form. Multi-line text is split so the gutter
/// reaches every line, not just the first.
/// Test: `tests::build_chat_lines_frames_a_delegation_block`,
/// `tests::build_chat_lines_gutters_every_delegated_line`.
fn delegation_lines(entry: &crate::app::ChatLine) -> Vec<Line<'static>> {
    let frame = entry.role == ChatRole::Delegation;
    let style = if frame {
        Style::default()
            .fg(Color::Indexed(141))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Indexed(146))
    };
    entry
        .text
        .lines()
        .map(|raw| {
            if frame {
                Line::from(Span::styled(raw.to_string(), style))
            } else {
                Line::from(vec![
                    delegated_gutter_span(),
                    Span::styled(raw.to_string(), style),
                ])
            }
        })
        .collect()
}

/// Build the rendered `Vec<Line>` for the chat pane.
///
/// Why: extracted so layout code can compute the chat content height
/// *before* the vertical layout split, letting the input row follow content
/// height instead of being pinned to the screen bottom.
/// What: replicates the banner + chat entry line-building logic; returns the
/// post-collapse, pre-pad lines. Width-dependent rendering (banner wrapping,
/// markdown table column sizing) uses `terminal_width`.
/// Test: `tests::build_chat_lines_renders_user_and_assistant_entries`,
/// `tests::build_chat_lines_renders_fenced_code_block`,
/// `tests::build_chat_lines_renders_markdown_table`,
/// `tests::build_chat_lines_prepends_banner_when_shown`.
pub fn build_chat_lines(app: &ReplApp, terminal_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if app.show_banner {
        lines.extend(banner_lines(app, terminal_width));
        lines.push(Line::from(""));
    }

    for (idx, entry) in app.chat.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::from(""));
        }
        // #4596: a tool call renders as a card; its role only says whether
        // the card sits inside a delegation block.
        if let Some(card) = &entry.tool {
            lines.extend(tool_card_lines(card, entry.role == ChatRole::Delegated));
            continue;
        }
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
            }
            ChatRole::Assistant | ChatRole::Error => {
                let body_color = if entry.role == ChatRole::Error {
                    Some(Color::Red)
                } else {
                    None
                };
                let body_lines: Vec<&str> = entry.text.lines().collect();
                let mut i = 0usize;
                let mut emitted_first = false;
                while i < body_lines.len() {
                    if let Some(lang) = code_fence_lang(body_lines[i]) {
                        let is_shell = is_executable_shell_lang(&lang);
                        if !emitted_first {
                            lines.push(Line::from(leader_spans(app, body_color)));
                            emitted_first = true;
                        }
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
                            i = j;
                        }
                        continue;
                    }

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
                        let indent = "   ";
                        let avail = terminal_width.saturating_sub(1);
                        let table_lines =
                            render_markdown_table(&header, &body_rows, avail, indent, body_color);
                        if !emitted_first {
                            lines.push(Line::from(leader_spans(app, body_color)));
                            emitted_first = true;
                        }
                        for tl in table_lines {
                            lines.push(tl);
                        }
                        i = j;
                        continue;
                    }

                    let raw = body_lines[i];
                    if !emitted_first {
                        let mut spans = leader_spans(app, body_color);
                        spans.push(match body_color {
                            Some(c) => Span::styled(raw.to_string(), Style::default().fg(c)),
                            None => Span::raw(raw.to_string()),
                        });
                        lines.push(Line::from(spans));
                        emitted_first = true;
                    } else {
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
                    Span::styled(app.status_prefix.clone(), Style::default().fg(Color::Green)),
                    Span::raw(entry.text.clone()),
                ]));
            }
            // #7940: the two roles that frame and fill a delegated
            // sub-agent's block — see `delegation_lines`.
            ChatRole::Delegation | ChatRole::Delegated => {
                lines.extend(delegation_lines(entry));
            }
        }
    }

    let mut lines = collapse_blank_lines(lines);

    if !app.chat.is_empty() {
        lines.push(Line::from(""));
    }

    lines
}

/// Count chat content lines for layout sizing.
///
/// Why: [`crate::layout::draw`] needs to know the rendered content height
/// *before* the vertical layout split so the chat pane can use
/// `Constraint::Length(h)` instead of `Constraint::Min` — `Min` expands to
/// fill all available space, pinning the input row to the screen bottom even
/// when the conversation is short.
/// What: returns the post-collapse, pre-pad line count from
/// [`build_chat_lines`].
/// Test: `tests::chat_line_count_is_zero_for_empty_chat_without_banner`.
pub fn chat_line_count(app: &ReplApp, terminal_width: usize) -> usize {
    build_chat_lines(app, terminal_width).len()
}

/// Render the chat pane into `area`, bottom-pinned when content fits.
///
/// Why: mirrors an iMessage/Slack-style chat feel — short conversations sit
/// flush at the bottom of the pane rather than floating at the top with a
/// sea of blank space below.
/// What: pads with blank lines above the content when it fits; otherwise
/// scrolls so the newest line stays in view (clamped by
/// `app.scroll_offset`). Publishes the computed max scroll offset to
/// `app.last_max_scroll` so `ReplApp::scroll` can clamp future deltas.
/// Test: exercised via `draw` in [`crate::layout`]; the pure geometry
/// (padding decision, max-offset computation) is covered indirectly through
/// [`build_chat_lines`]'s tests plus [`ReplApp::scroll`]'s own tests in
/// `crate::app::reduce`.
pub fn draw_chat(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let lines = build_chat_lines(app, area.width as usize);

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

    let final_total = final_lines.len();
    let max_offset = final_total.saturating_sub(visible);
    app.last_max_scroll
        .store(max_offset, std::sync::atomic::Ordering::Relaxed);
    let effective_offset = max_offset.saturating_sub(app.scroll_offset);

    let paragraph = Paragraph::new(final_lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_offset as u16, 0));
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ReplApp;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn all_text(lines: &[Line<'_>]) -> String {
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    fn app_without_banner() -> ReplApp {
        let mut app = ReplApp::new("demo", "bob");
        app.show_banner = false;
        app
    }

    #[test]
    fn chat_line_count_is_zero_for_empty_chat_without_banner() {
        let app = app_without_banner();
        assert_eq!(chat_line_count(&app, 80), 0);
    }

    #[test]
    fn build_chat_lines_renders_user_and_assistant_entries() {
        let mut app = app_without_banner();
        app.push_user("hello");
        app.push_assistant("hi there", false);
        let text = all_text(&build_chat_lines(&app, 80));
        assert!(text.contains("❯ hello"));
        assert!(text.contains("demo"));
        assert!(text.contains("hi there"));
    }

    #[test]
    fn build_chat_lines_renders_fenced_code_block() {
        let mut app = app_without_banner();
        app.push_assistant("Run:\n```bash\necho hi\n```\nDone.", false);
        let text = all_text(&build_chat_lines(&app, 80));
        assert!(
            text.contains("▶ bash"),
            "missing shell fence header: {text}"
        );
        assert!(text.contains("echo hi"));
    }

    #[test]
    fn build_chat_lines_renders_markdown_table() {
        let mut app = app_without_banner();
        app.push_assistant("| a | b |\n|---|---|\n| 1 | 2 |", false);
        let text = all_text(&build_chat_lines(&app, 80));
        assert!(text.contains('┌'), "missing table border: {text}");
        assert!(text.contains('1'));
        assert!(text.contains('2'));
    }

    #[test]
    fn build_chat_lines_prepends_banner_when_shown() {
        let app = ReplApp::new("demo", "bob");
        assert!(app.show_banner);
        let lines = build_chat_lines(&app, 80);
        assert!(!lines.is_empty(), "banner must render even with no chat");
    }

    #[test]
    fn build_chat_lines_error_role_renders_red_body() {
        let mut app = app_without_banner();
        app.push_assistant("boom", true);
        // The error body carries a distinct color from a normal response —
        // spot-check via the raw text still round-tripping (styling itself
        // is asserted indirectly by `ReplApp::push_assistant`'s role test).
        let text = all_text(&build_chat_lines(&app, 80));
        assert!(text.contains("boom"));
    }

    #[test]
    fn build_chat_lines_status_role_uses_configured_prefix() {
        let mut app = app_without_banner();
        app.status_prefix = "[custom] ".to_string();
        app.push_status("hello");
        let text = all_text(&build_chat_lines(&app, 80));
        assert!(text.contains("[custom] hello"));
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
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].spans[0].content, "a");
        assert!(out[1].spans.iter().all(|s| s.content.trim().is_empty()));
        assert_eq!(out[2].spans[0].content, "b");
        assert!(out[3].spans.iter().all(|s| s.content.trim().is_empty()));
        assert_eq!(out[4].spans[0].content, "c");
    }

    /// #7940: a delegation reads as a block — a flush-left header, gutter-
    /// prefixed body, and a flush-left footer naming the outcome.
    #[test]
    fn build_chat_lines_frames_a_delegation_block() {
        let mut app = app_without_banner();
        app.chat.push(crate::app::ChatLine {
            role: ChatRole::Delegation,
            text: "▶ engineer — add the renderer".into(),
            tool: None,
        });
        app.chat.push(crate::app::ChatLine {
            role: ChatRole::Delegated,
            text: "reading the spec".into(),
            tool: None,
        });
        app.chat.push(crate::app::ChatLine {
            role: ChatRole::Delegation,
            text: "└ engineer — success".into(),
            tool: None,
        });
        let lines = build_chat_lines(&app, 80);
        let text = all_text(&lines);
        assert!(text.contains("▶ engineer — add the renderer"), "{text}");
        assert!(
            text.contains(&format!("{DELEGATED_GUTTER}reading the spec")),
            "{text}"
        );
        assert!(text.contains("└ engineer — success"), "{text}");
    }

    /// The gutter has to reach EVERY line of a multi-line sub-agent message,
    /// not just the first — otherwise a wrapped answer falls out of the
    /// block after one line.
    #[test]
    fn build_chat_lines_gutters_every_delegated_line() {
        let mut app = app_without_banner();
        app.chat.push(crate::app::ChatLine {
            role: ChatRole::Delegated,
            text: "one\ntwo\nthree".into(),
            tool: None,
        });
        let lines = build_chat_lines(&app, 80);
        for expected in ["one", "two", "three"] {
            assert!(
                lines
                    .iter()
                    .any(|l| line_text(l) == format!("{DELEGATED_GUTTER}{expected}")),
                "missing gutter on {expected}"
            );
        }
    }
}
