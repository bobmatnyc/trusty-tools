//! The startup banner — generalized port of tagent's `repl/tui/banner.rs`.
//!
//! Why: DOC-50 §5 Slice 4 migrates the banner's two-column framed layout
//! (splash-art-or-identity on the left, title/activity/commands on the
//! right) since the *layout* is fully generic. What did NOT migrate is the
//! content: tagent's original hard-codes the shared `trusty_common::banner`
//! splash art, the literal title `"trusty-agents ctrl"`, and a fixed
//! `/help`/`/connect`/`/clear`/`/status` command list. Depending on
//! `trusty_common` here would violate DOC-50 §2.2's dependency direction
//! (`trusty-code-tui` → ratatui/crossterm/tokio/serde only) and hard-coding any
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
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): migrate `banner.rs`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::ReplApp;
use crate::text::elide_middle;

/// Maximum number of [`ReplApp::recent_activity`] entries shown.
const MAX_RECENT_ACTIVITY: usize = 3;

/// Maximum number of [`ReplApp::commands`] entries shown (the banner is a
/// glance-able splash, not a full `/help` listing).
const MAX_COMMANDS: usize = 4;

/// Leading whitespace on a wrapped splash row, so a continuation reads as
/// part of the line above it rather than as a new fact.
const SPLASH_CONTINUATION_INDENT: &str = "   ";

/// Floor on the wrap width a splash row is given.
///
/// Why: the right column is not derived from the terminal width alone —
/// `left_w` is widened to fit [`ReplApp::banner_art`] unclipped, so art
/// wider than the frame squeezes the right column to a handful of columns.
/// Below this floor [`crate::text::elide_middle`] shortens
/// every word of a splash row to a two-character stub, so the launch facts
/// render as punctuation rather than as words. Shaping the rows for 12
/// columns and letting the renderer clip them is the better failure: what
/// survives is then a readable prefix.
/// Test: `tests::banner_lines_keeps_splash_words_when_the_right_column_collapses`.
const MIN_SPLASH_WIDTH: usize = 12;

/// Break one splash line into rows that each fit `width` columns.
///
/// Why (#8164): the right column is `width - width/4 - 3` columns wide, so an
/// 80-column terminal gives it ~57 — less than a build-mismatch warning or an
/// absolute project path needs. The surrounding renderer CLIPS a row's tail,
/// which is the half of those two lines carrying the daemon's sha, the remedy,
/// and a path's final components. Wrapping keeps every character.
/// What: greedy word wrap on whitespace; a single token too long to fit on a
/// row of its own is middle-elided by [`crate::text::elide_middle`] rather
/// than clipped, so a path keeps its deepest components whole. Always returns
/// at least one row.
/// Test: `tests::fit_splash_line_wraps_rather_than_clipping`,
/// `tests::fit_splash_line_elides_the_middle_of_an_unbreakable_token`,
/// `tests::banner_lines_keep_the_mismatch_warning_readable_at_80_columns`.
fn fit_splash_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        let word = elide_middle(word, width);
        let current_len = current.chars().count();
        if current_len > 0 && current_len + 1 + word.chars().count() > width {
            rows.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&word);
    }
    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    rows
}

/// Produce the welcome banner as a `Vec<Line<'static>>` so it can be
/// prepended to the chat scroll buffer instead of occupying its own layout
/// chunk.
///
/// Why: treating the banner as part of the chat scrollback lets it scroll
/// upward (and eventually off the top) as new messages arrive, exactly like
/// terminal command history — avoiding an abrupt "banner disappears on
/// first message" UX.
/// What: left column is `app.banner_art` (if any) plus the
/// `{user_label} · {label}` identity line; right column is the title (or
/// `app.splash` in its place, #8164), recent activity, and commands. Column
/// widths are computed from `width`, widened enough to fit the art (if
/// present) unclipped.
/// Test: `tests::banner_lines_top_and_bottom_rules_present`,
/// `tests::banner_lines_includes_identity_and_title`,
/// `tests::banner_lines_lists_recent_activity_and_commands`,
/// `tests::banner_lines_renders_with_no_art_or_commands`,
/// `tests::banner_lines_splash_replaces_the_identity_row`,
/// `tests::banner_lines_keep_the_mismatch_warning_readable_at_80_columns`,
/// `tests::banner_lines_keeps_splash_words_when_the_right_column_collapses`.
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

    let header = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut right_rows: Vec<(String, Style)> = Vec::new();
    // #8164: an engine-supplied splash REPLACES the generic identity row —
    // its own first line is the header, so keeping both would print the
    // product's name and version twice.
    if app.splash.is_empty() {
        right_rows.push((format!(" {} v{}", app.banner_title, app.version), header));
    } else {
        // #8164: splash rows WRAP rather than clip — the right column is only
        // ~57 columns on an 80-column terminal, and the tail of a build
        // mismatch warning (the daemon's sha and what to do about it) is the
        // part that matters most.
        let wrap_w = right_w
            .saturating_sub(SPLASH_CONTINUATION_INDENT.len())
            .max(MIN_SPLASH_WIDTH);
        for (idx, line) in app.splash.iter().enumerate() {
            let style = if idx == 0 { header } else { Style::default() };
            for (row, text) in fit_splash_line(line, wrap_w).into_iter().enumerate() {
                let indent = if row == 0 {
                    " "
                } else {
                    SPLASH_CONTINUATION_INDENT
                };
                right_rows.push((format!("{indent}{text}"), style));
            }
        }
    }
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

    /// A splash must REPLACE the `{banner_title} v{version}` row, not stack
    /// with it (#8164): the product states its identity once, in its own
    /// words.
    #[test]
    fn banner_lines_splash_replaces_the_identity_row() {
        let mut app = ReplApp::new("tcode", "bob");
        app.version = "9.9.9".to_string();
        app.splash = vec![
            "🤖🤖🤖 tcode v0.7.0 (ea6a1a9e 2026-09-16)".to_string(),
            "project /repo/x".to_string(),
        ];
        let text: String = banner_lines(&app, 120)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("🤖🤖🤖 tcode v0.7.0"), "{text}");
        assert!(text.contains("project /repo/x"), "{text}");
        // The top RULE still carries the title (that is the frame, not the
        // content column); only the right column's identity row is replaced.
        let rows: Vec<&str> = text.lines().skip(1).collect();
        assert!(
            !rows.iter().any(|r| r.contains("tcode v9.9.9")),
            "the generic identity row must be gone: {text}"
        );
    }

    /// #8164: on an 80-column terminal the right column is ~57 wide, so a
    /// build-mismatch warning and a real project path BOTH overflow it. The
    /// facts that must survive are the daemon's sha, the remedy, and the
    /// path's final components — exactly what a tail clip destroys.
    #[test]
    fn banner_lines_keep_the_mismatch_warning_readable_at_80_columns() {
        let mut app = ReplApp::new("tcode", "masa");
        app.splash = vec![
            "🤖🤖🤖 tcode v0.7.0 (ea6a1a9e 2026-09-16)".to_string(),
            "project /Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.claude/worktrees/agent-af14d84dd34577cee".to_string(),
            "warning: daemon build deadbeef ≠ client ea6a1a9e — restart the daemon".to_string(),
        ];
        let lines = banner_lines(&app, 80);
        let text: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(
            text.contains("deadbeef"),
            "the daemon sha must survive: {text}"
        );
        assert!(
            text.contains("ea6a1a9e"),
            "the client sha must survive: {text}"
        );
        assert!(
            text.contains("restart the daemon"),
            "the remedy must survive: {text}"
        );
        // The deepest components arrive WHOLE — the worktree's own name and
        // the directory holding it. Components nearer the root are dropped
        // first, which is the guarantee `text::elide_middle` documents.
        assert!(
            text.contains("worktrees/agent-af14d84dd34577cee"),
            "the worktree's own name must survive whole: {text}"
        );
        // Wrapping must not widen the frame: every row still fits 80 columns.
        for line in &lines {
            assert!(
                line_text(line).chars().count() <= 80,
                "a wrapped row must not overflow the frame: {}",
                line_text(line)
            );
        }
    }

    /// `MIN_SPLASH_WIDTH` is not reachable through `width` alone — with no
    /// art, `total_w = width.max(40)` and `left_w = total_w / 4` keep
    /// `right_w >= 27`. It is reached when `banner_art` is WIDER than the
    /// frame, because `left_w` is widened to fit the art unclipped and what
    /// remains for the right column is a handful of columns. Without the
    /// floor, every word of the splash is elided to a two-character stub
    /// before the renderer ever clips the row.
    #[test]
    fn banner_lines_keeps_splash_words_when_the_right_column_collapses() {
        let mut app = ReplApp::new("tcode", "masa");
        // 27 columns of art on a 40-column frame: left_w = 29, right_w = 8,
        // so the un-floored wrap width would be 5.
        app.banner_art = vec![Line::from("x".repeat(27))];
        app.splash = vec!["tcode v0.7.0".to_string(), "project /repo".to_string()];
        let text: String = banner_lines(&app, 40)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("project"),
            "a splash word must survive whole: {text}"
        );
        assert!(
            text.contains("tcode v"),
            "the header row must stay readable: {text}"
        );
        assert!(
            !text.contains('…'),
            "words that fit 12 columns must not be elided: {text}"
        );
    }

    /// A line longer than the column becomes more rows, never a clipped one.
    #[test]
    fn fit_splash_line_wraps_rather_than_clipping() {
        let rows = fit_splash_line("alpha bravo charlie delta", 12);
        assert!(rows.len() > 1, "{rows:?}");
        assert_eq!(rows.join(" "), "alpha bravo charlie delta");
        for row in &rows {
            assert!(row.chars().count() <= 12, "{row:?}");
        }
    }

    /// A path has no spaces to wrap on, so it is elided on `/` boundaries —
    /// the repository name at the end is the component a reader is checking.
    #[test]
    fn fit_splash_line_elides_the_middle_of_an_unbreakable_token() {
        let rows = fit_splash_line("/Users/masa/trusty-mpm-projects/bobmatnyc/bakeoff-l1", 24);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].chars().count() <= 24, "{rows:?}");
        assert!(rows[0].ends_with("bobmatnyc/bakeoff-l1"), "{rows:?}");
        assert!(rows[0].contains('…'), "{rows:?}");
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
