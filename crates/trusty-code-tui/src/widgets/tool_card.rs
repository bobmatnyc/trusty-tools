//! Tool-call cards in the chat scrollback (#4596).
//!
//! Why: a tool call and its result used to render as two flat status lines,
//! and the result's line breaks were lost, so `read_file` and `cargo test`
//! output collapsed onto one unreadable row. A card keeps a call and its
//! result together as one unit and keeps every line of the result.
//! What: [`tool_card_lines`] turns one [`ToolCard`] into scrollback lines;
//! [`crate::widgets::scrollback::build_chat_lines`] calls it for every chat
//! entry that carries a card.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 8 deliverable (§5, Slice 8): tool invocation rendering.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::ToolCard;
use crate::widgets::scrollback::delegated_gutter_span;

/// Prefix of a card's first result line, tying the output to the call above.
const RESULT_ELBOW: &str = "  ⎿  ";
/// Prefix of every later result line, aligned under the first.
const RESULT_INDENT: &str = "     ";

/// Render one tool card: a header naming the tool and its arguments, then
/// the result with its line breaks intact.
///
/// Why: the header and body are separate rows so a multi-line result reads
/// as the output it is, not as one wrapped string.
/// What: the header is `⏺ <tool>(<args>)`, the glyph yellow while the call
/// runs and green once a result arrives. The body is `running…` while
/// pending, `(no output)` for an empty result, and otherwise one row per
/// result line. When `delegated`, every row carries the delegation gutter so
/// the card sits inside its sub-agent's block (#7940).
/// Test: `tests::tool_call_and_result_render_as_one_card`,
/// `tests::multi_line_result_keeps_its_line_breaks`,
/// `tests::delegated_card_is_guttered_and_top_level_card_is_not`.
pub fn tool_card_lines(card: &ToolCard, delegated: bool) -> Vec<Line<'static>> {
    let glyph = if card.result.is_some() {
        Color::Green
    } else {
        Color::Yellow
    };
    let mut header = vec![
        Span::styled("⏺ ", Style::default().fg(glyph)),
        Span::styled(
            card.tool_name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    let args = args_summary(&card.args);
    if !args.is_empty() {
        header.push(Span::styled(
            format!("({args})"),
            Style::default().add_modifier(Modifier::DIM),
        ));
    }

    let dim = Style::default().add_modifier(Modifier::DIM);
    let body = Style::default().fg(Color::Indexed(244));
    let mut rows = vec![header];
    match card.result.as_deref() {
        None => rows.push(vec![Span::raw(RESULT_ELBOW), Span::styled("running…", dim)]),
        Some(r) if r.trim().is_empty() => rows.push(vec![
            Span::raw(RESULT_ELBOW),
            Span::styled("(no output)", dim),
        ]),
        Some(r) => {
            for (i, raw) in r.lines().enumerate() {
                let prefix = if i == 0 { RESULT_ELBOW } else { RESULT_INDENT };
                rows.push(vec![Span::raw(prefix), Span::styled(raw.to_string(), body)]);
            }
        }
    }

    rows.into_iter()
        .map(|mut spans| {
            if delegated {
                spans.insert(0, delegated_gutter_span());
            }
            Line::from(spans)
        })
        .collect()
}

/// One-line text for a call's arguments: a JSON string shows unquoted,
/// `Null` shows nothing, and anything else shows as compact JSON. Line
/// breaks fold into spaces so the header stays one row before wrapping.
fn args_summary(args: &serde_json::Value) -> String {
    let text = match args {
        serde_json::Value::Null => return String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
