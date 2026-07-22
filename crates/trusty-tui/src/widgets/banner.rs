//! The startup banner — generalized port of tagent's `repl/tui/banner.rs`.
//!
//! Why: DOC-50 §5 Slice 4 migrates the banner's two-column framed layout
//! (splash-art-or-identity on the left, title/activity/commands on the
//! right) since the *layout* is fully generic. What did NOT migrate is the
//! content: tagent's original hard-codes the shared `trusty_common::banner`
//! splash art, the literal title `"trusty-agents ctrl"`, and a fixed
//! `/help`/`/connect`/`/clear`/`/status` command list. Depending on
//! `trusty_common` here would violate DOC-50 §2.2's dependency direction
//! (`trusty-tui` → ratatui/crossterm/tokio/serde only) and hard-coding any
//! product's commands would violate the generalization mandate (§3.2) — so
//! every one of those became an [`crate::app::ReplApp`] field a product sets
//! at construction: [`ReplApp::banner_art`] (pre-rendered, empty by default),
//! [`ReplApp::banner_title`]/[`ReplApp::version`], and
//! [`ReplApp::recent_activity`]/[`ReplApp::commands`] (the latter sourced
//! from [`crate::engine::TuiEngine::commands`]).
//!
//! What: [`banner_lines`] returns the same two-column, rounded-corner-framed
//! `Vec<Line>` tagent's `banner_lines` produced, now built entirely from
//! `ReplApp` fields instead of hard-coded content.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): migrate `banner.rs`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::ReplApp;

/// Maximum number of [`ReplApp::recent_activity`] entries shown.
const MAX_RECENT_ACTIVITY: usize = 3;

/// Maximum number of [`ReplApp::commands`] entries shown (the banner is a
/// glance-able splash, not a full `/help` listing).
const MAX_COMMANDS: usize = 4;

/// Produce the welcome banner as a `Vec<Line<'static>>` so it can be
/// prepended to the chat scroll buffer instead of occupying its own layout
/// chunk.
///
/// Why: treating the banner as part of the chat scrollback lets it scroll
/// upward (and eventually off the top) as new messages arrive, exactly like
/// terminal command history — avoiding an abrupt "banner disappears on
/// first message" UX.
/// What: left column is `app.banner_art` (if any) plus the
/// `{user_label} · {label}` identity line; right column is the title,
/// recent activity, and commands. Column widths are computed from `width`,
/// widened enough to fit the art (if present) unclipped.
/// Test: [`tests::banner_lines_top_and_bottom_rules_present`],
/// [`tests::banner_lines_includes_identity_and_title`],
/// [`tests::banner_lines_lists_recent_activity_and_commands`],
/// [`tests::banner_lines_renders_with_no_art_or_commands`].
pub fn banner_lines(app: &ReplApp, width: usize) -> Vec<Line<'static>> {
    let art_width = app
        .banner_art
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum())
        .max()
        .unwrap_or(0usize);

    let total_w = width.max(40);
    let left_w = (total_w / 4).max(art_width + 2);
    let div_w = 1usize;
    let right_w = total_w.saturating_sub(left_w + div_w + 2);

    let mut left_rows: Vec<Vec<Span<'static>>> =
        app.banner_art.iter().map(|l| l.spans.clone()).collect();
    left_rows.push(Vec::new());
    left_rows.push(vec![Span::raw(format!(
        "{} · {}",
        app.user_label, app.label
    ))]);

    let mut right_rows: Vec<(String, Style)> = Vec::new();
    right_rows.push((
        format!(" {} v{}", app.banner_title, app.version),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    right_rows.push((String::new(), Style::default()));
    right_rows.push((
        " Recent activity".to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    for entry in app.recent_activity.iter().take(MAX_RECENT_ACTIVITY) {
        right_rows.push((format!(" {}", entry), Style::default()));
    }
    while right_rows.len() < 7 {
        right_rows.push((String::new(), Style::default()));
    }
    right_rows.push((
        " Commands".to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    for cmd in app.commands.iter().take(MAX_COMMANDS) {
        right_rows.push((
            format!("   /{:<12} - {}", cmd.name, cmd.summary),
            Style::default(),
        ));
    }

    let row_count = left_rows.len().max(right_rows.len());
    let mut out: Vec<Line<'static>> = Vec::with_capacity(row_count + 2);

    let title = format!("╭─── {} v{} ", app.banner_title, app.version);
    let mut top = String::with_capacity(total_w);
    top.push_str(&title);
    while top.chars().count() + 1 < total_w {
        top.push('─');
    }
    top.push('╮');
    out.push(Line::from(Span::styled(
        top,
        Style::default().fg(Color::Cyan),
    )));

    for i in 0..row_count {
        let left_spans = left_rows.get(i).cloned().unwrap_or_default();
        let (right_raw, right_style) = right_rows
            .get(i)
            .cloned()
            .unwrap_or_else(|| (String::new(), Style::default()));

        let left_chars: usize = left_spans.iter().map(|s| s.content.chars().count()).sum();
        let pad_total = left_w.saturating_sub(left_chars);
        let pad_left = pad_total / 2;
        let pad_right = pad_total - pad_left;

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(left_spans.len() + 4);
        if pad_left > 0 {
            spans.push(Span::raw(" ".repeat(pad_left)));
        }
        spans.extend(left_spans);
        if pad_right > 0 {
            spans.push(Span::raw(" ".repeat(pad_right)));
        }
        spans.push(Span::styled(" ", Style::default()));
        spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(" ", Style::default()));

        let right_truncated: String = right_raw.chars().take(right_w).collect();
        spans.push(Span::styled(right_truncated, right_style));

        out.push(Line::from(spans));
    }

    let mut bottom = String::with_capacity(total_w);
    bottom.push('╰');
    while bottom.chars().count() + 1 < total_w {
        bottom.push('─');
    }
    bottom.push('╯');
    out.push(Line::from(Span::styled(
        bottom,
        Style::default().fg(Color::DarkGray),
    )));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommandDescriptor, CommandRouting};

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn banner_lines_top_and_bottom_rules_present() {
        let app = ReplApp::new("demo-project", "bob");
        let lines = banner_lines(&app, 120);
        let top = line_text(lines.first().expect("banner must have a top rule"));
        let bottom = line_text(lines.last().expect("banner must have a bottom rule"));
        assert!(top.starts_with("╭─── demo-project"));
        assert!(bottom.starts_with('╰'));
    }

    #[test]
    fn banner_lines_includes_identity_and_title() {
        let app = ReplApp::new("demo-project", "bob");
        let text: String = banner_lines(&app, 120)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("bob · demo-project"));
        assert!(text.contains("demo-project v"));
    }

    #[test]
    fn banner_lines_lists_recent_activity_and_commands() {
        let mut app = ReplApp::new("demo-project", "bob");
        app.recent_activity = vec!["fixed the bug".to_string()];
        app.commands = vec![CommandDescriptor {
            name: "workstream".to_string(),
            summary: "List or activate a workstream".to_string(),
            routing: CommandRouting::Engine,
            args_hint: None,
        }];
        let text: String = banner_lines(&app, 120)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("fixed the bug"));
        assert!(text.contains("/workstream"));
        assert!(text.contains("List or activate a workstream"));
    }

    #[test]
    fn banner_lines_renders_with_no_art_or_commands() {
        // Zero-config case: no banner_art, no commands, no recent_activity.
        // Must still render a well-formed frame (this crate ships no
        // branding assets of its own — see the module doc comment).
        let app = ReplApp::new("demo-project", "bob");
        let lines = banner_lines(&app, 60);
        assert!(
            lines.len() > 2,
            "must render more than just top/bottom rules"
        );
    }
}
