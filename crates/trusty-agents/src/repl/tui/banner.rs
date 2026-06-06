//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Produce the welcome banner as a `Vec<Line<'static>>` so it can be prepended
/// to the chat scroll buffer instead of occupying its own layout chunk.
///
/// Why: Treating the banner as part of the chat scrollback lets it scroll
/// upward (and eventually off the top) as new messages arrive — exactly like
/// terminal command history. This eliminates the abrupt "banner disappears
/// when first message is sent" UX from the previous design.
/// What: Returns the same content `draw_banner()` rendered as widgets, but as
/// raw `Line`s. Layout: left ASCII-art column, vertical `│` divider, right
/// info column. Column widths are computed from `width` (25% / 1 / rest).
/// Test: Visual via `scripts/tmux-repl-test.sh`; mechanical via the
/// `banner_lines_*` unit tests.
pub(crate) fn banner_lines(app: &ReplApp, width: usize) -> Vec<Line<'static>> {
    let version = env!("CARGO_PKG_VERSION");

    let total_w = width.max(40);
    let left_w = (total_w / 4).max(18);
    let div_w = 1usize;
    let right_w = total_w.saturating_sub(left_w + div_w + 2 /* margins */);

    // Left column rows (centered ASCII art + identity).
    let left_rows: Vec<(String, Option<Color>)> = vec![
        (String::new(), None),
        (String::new(), None),
        ("▐▛███▜▌ ▐▛███▜▌".to_string(), Some(Color::Cyan)),
        ("▝▜█████▛▘▝▜█████▛▘".to_string(), Some(Color::Cyan)),
        ("▘▘ ▝▝    ▘▘ ▝▝".to_string(), Some(Color::Cyan)),
        (String::new(), None),
        (format!("{} · {}", app.user_label, app.project_name), None),
    ];

    // Right column rows: app title + recent activity + commands.
    let mut right_rows: Vec<(String, Style)> = Vec::new();
    right_rows.push((
        format!(" Open MPM v{}", version),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    right_rows.push((String::new(), Style::default()));
    right_rows.push((
        " Recent activity".to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    for c in app.git_commits.iter().take(3) {
        right_rows.push((format!(" {}", c), Style::default()));
    }
    while right_rows.len() < 7 {
        right_rows.push((String::new(), Style::default()));
    }
    right_rows.push((
        " Commands".to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    right_rows.push((
        "   /help       - show all commands".to_string(),
        Style::default(),
    ));
    right_rows.push((
        "   /connect    - attach to a project".to_string(),
        Style::default(),
    ));
    right_rows.push((
        "   /clear      - reset conversation".to_string(),
        Style::default(),
    ));
    right_rows.push((
        "   /status     - show agent status".to_string(),
        Style::default(),
    ));

    let row_count = left_rows.len().max(right_rows.len());
    let mut out: Vec<Line<'static>> = Vec::with_capacity(row_count + 2);

    // Top rule. Uses a rounded-corner frame (`╭─── … ─╮`) so the banner reads
    // as a self-contained frame rather than fading into the chat scroll. The
    // tmux e2e test (`scripts/tmux-repl-test.sh`) asserts the literal
    // `╭─── trusty-agents ctrl` substring, so the leading `╭───` and the title
    // format must be preserved verbatim.
    let title = format!("╭─── trusty-agents ctrl  v{} ", version);
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

    // Body rows.
    for i in 0..row_count {
        let (left_raw, left_color) = left_rows
            .get(i)
            .cloned()
            .unwrap_or_else(|| (String::new(), None));
        let (right_raw, right_style) = right_rows
            .get(i)
            .cloned()
            .unwrap_or_else(|| (String::new(), Style::default()));

        // Center left text within left_w.
        let left_chars = left_raw.chars().count();
        let pad_total = left_w.saturating_sub(left_chars);
        let pad_left = pad_total / 2;
        let pad_right = pad_total - pad_left;
        let left_padded = format!(
            "{}{}{}",
            " ".repeat(pad_left),
            left_raw,
            " ".repeat(pad_right)
        );
        let left_style = match left_color {
            Some(c) => Style::default().fg(c),
            None => Style::default(),
        };

        // Truncate right to right_w.
        let right_truncated: String = right_raw.chars().take(right_w).collect();

        let spans = vec![
            Span::styled(left_padded, left_style),
            Span::styled(" ", Style::default()),
            Span::styled("│", Style::default().fg(Color::DarkGray)),
            Span::styled(" ", Style::default()),
            Span::styled(right_truncated, right_style),
        ];
        out.push(Line::from(spans));
    }

    // Bottom rule: closes the banner with a rounded-corner frame matching
    // the top bar, so the splash reads as a bounded box rather than fading
    // into the chat scroll. The tmux e2e test (`scripts/tmux-repl-test.sh`)
    // asserts the literal `╰─` opener.
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

#[allow(dead_code)]
pub(crate) fn draw_banner(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    // Use the compile-time crate version so the banner always matches the
    // shipped binary without an extra build step.
    let version = env!("CARGO_PKG_VERSION");
    // Title format mirrors the legacy banner so tmux test assertions
    // (`╭─── trusty-agents ctrl`) keep working.
    let title = format!("─── trusty-agents ctrl  v{} ", version);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(title, Style::default().fg(Color::Cyan)));

    let inner = block.inner(area);

    // Three-column inner layout: 25% identity / 1-char vertical divider /
    // remainder for activity+commands. The divider visually splits the
    // ASCII robot art on the left from the right-side content.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    f.render_widget(block, area);

    // Left panel.
    let left_lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "▐▛███▜▌ ▐▛███▜▌",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "▝▜█████▛▘▝▜█████▛▘",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "▘▘ ▝▝    ▘▘ ▝▝",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(format!("{} · {}", app.user_label, app.project_name)),
    ];
    let left = Paragraph::new(left_lines).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(left, cols[0]);

    // Vertical divider column: render a `│` glyph for each row so it spans
    // the full height of the banner inner area. Subtle dim grey so it reads
    // as a separator without competing with the cyan border.
    let divider_lines: Vec<Line<'static>> = (0..cols[1].height)
        .map(|_| Line::from(Span::styled("│", Style::default().fg(Color::DarkGray))))
        .collect();
    let divider = Paragraph::new(divider_lines);
    f.render_widget(divider, cols[1]);

    // Right panel: app title header, recent activity, commands.
    let mut right_lines: Vec<Line> = Vec::with_capacity(12);
    // App title — prominent at the top of the right column.
    right_lines.push(Line::from(Span::styled(
        format!(" Open MPM v{}", version),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    right_lines.push(Line::from(""));
    right_lines.push(Line::from(Span::styled(
        " Recent activity",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for c in app.git_commits.iter().take(3) {
        right_lines.push(Line::from(format!(" {}", c)));
    }
    while right_lines.len() < 7 {
        right_lines.push(Line::from(""));
    }
    right_lines.push(Line::from(Span::styled(
        " Commands",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    right_lines.push(Line::from("   /help       - show all commands"));
    right_lines.push(Line::from("   /connect    - attach to a project"));
    right_lines.push(Line::from("   /clear      - reset conversation"));
    right_lines.push(Line::from("   /status     - show agent status"));
    let right = Paragraph::new(right_lines).wrap(Wrap { trim: false });
    f.render_widget(right, cols[2]);
}
