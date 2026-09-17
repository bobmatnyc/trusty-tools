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
/// Longest argument preview a collapsed header shows before eliding (#4596)
/// — the summary must stay one row on a narrow terminal.
const COLLAPSED_ARGS_MAX: usize = 32;

/// Render one tool card: a header naming the tool and its arguments, then
/// the result with its line breaks intact.
///
/// Why: the header and body are separate rows so a multi-line result reads
/// as the output it is, not as one wrapped string.
/// What: the header is `⏺ <tool>(<args>)`, the glyph yellow while the call
/// runs, green once a result arrives, and red when that result is an error
/// ([`ToolCard::is_error`]). The body is `running…` while pending,
/// `(no output)` for an empty result, and otherwise one row per result line.
/// A [`ToolCard::collapsed`] card drops the body entirely and appends a
/// status word plus a result-line count to the header, so it occupies one
/// row (#4596). When `delegated`, every row carries the delegation gutter so
/// the card sits inside its sub-agent's block (#7940).
/// Test: `tests::tool_call_and_result_render_as_one_card`,
/// `tests::completed_card_collapses_to_a_one_line_summary`,
/// `tests::errored_card_renders_expanded_by_default`,
/// `tests::multi_line_result_keeps_its_line_breaks`,
/// `tests::delegated_card_is_guttered_and_top_level_card_is_not`.
pub fn tool_card_lines(card: &ToolCard, delegated: bool) -> Vec<Line<'static>> {
    let glyph = match (card.result.is_some(), card.is_error()) {
        (_, true) => Color::Red,
        (true, false) => Color::Green,
        (false, false) => Color::Yellow,
    };
    let mut header = vec![
        Span::styled("⏺ ", Style::default().fg(glyph)),
        Span::styled(
            card.tool_name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    let args = args_summary(&card.args);
    let dim = Style::default().add_modifier(Modifier::DIM);
    if !args.is_empty() {
        let shown = if card.collapsed {
            elide(&args, COLLAPSED_ARGS_MAX)
        } else {
            args
        };
        header.push(Span::styled(format!("({shown})"), dim));
    }

    // #4596: a collapsed card is the header alone, carrying the status the
    // body would otherwise have shown. No duration is rendered — the seam
    // (`ReplEvent::ToolInvocation`) carries no timing, and inventing one
    // from arrival time would measure the TUI, not the tool.
    if card.collapsed {
        header.push(Span::styled(collapsed_status(card), dim));
        return vec![gutter(header, delegated)];
    }

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
        .map(|spans| gutter(spans, delegated))
        .collect()
}

/// Prepend the delegation gutter to one card row when the card belongs to a
/// sub-agent's block (#7940), then seal it into a `Line`.
fn gutter(mut spans: Vec<Span<'static>>, delegated: bool) -> Line<'static> {
    if delegated {
        spans.insert(0, delegated_gutter_span());
    }
    Line::from(spans)
}

/// The trailing text a collapsed card's header carries in place of its body
/// (#4596): ` · error`, ` · running…`, or ` · ok` plus the number of result
/// lines hidden (omitted for a single-line or empty result, where the count
/// says nothing).
fn collapsed_status(card: &ToolCard) -> String {
    if card.is_error() {
        return " · error".to_string();
    }
    let Some(result) = card.result.as_deref() else {
        return " · running…".to_string();
    };
    if result.trim().is_empty() {
        return " · ok · no output".to_string();
    }
    match result.lines().count() {
        0 | 1 => " · ok".to_string(),
        n => format!(" · ok · {n} lines"),
    }
}

/// Truncate `text` to at most `max` chars, marking the cut with `…`.
/// Char-based, so a multi-byte argument can never be split mid-character.
fn elide(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
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
