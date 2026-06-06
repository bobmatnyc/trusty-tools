//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Render the bottom rich statusline row.
///
/// Why: Replaces the dim `User`/segment-string statusline with a high-signal
/// info strip (mirroring Claude Code) showing harness identity, LLM provider
/// + model, tool/skill/MCP counts, and a hint to `/help`. Users want to see
/// at-a-glance what the next dispatch will use without scraping logs.
/// What: Bracketed `[trusty-agents]` (cyan/bold) + green `✓` + `LLM:` label
/// (normal) + `provider (model)` (bold) + dim `·` separators + `Tools/Skills/
/// MCP` counts + dim trailer. Falls back to the plain `render_statusline`
/// (User/segment) string if `app.status_line` is unset (defensive).
/// Test: Visual via `scripts/tmux-repl-test.sh` (asserts "All systems go" appears).
pub(crate) fn draw_statusline(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let line = build_rich_statusline(app);
    let p = Paragraph::new(line);
    f.render_widget(p, area);
}

/// Compose the rich statusline as a styled `Line`.
///
/// Why: Pulled out of `draw_statusline` so unit tests can assert the span
/// composition without a Terminal. The function is span-explicit (rather than
/// re-parsing `status_line`) because the underlying counts already live on
/// `ReplApp` (well — the original status string does). To keep the change
/// minimal and avoid plumbing new fields, we use `app.status_line` as the
/// source of truth: it's built once at startup with the exact counts and
/// the format is stable. We only re-style its parts.
/// What: If `status_line` is `Some`, return the bracketed `[trusty-agents] <body>`
/// styled line. Otherwise fall back to the legacy `render_statusline`.
/// Test: `rich_statusline_renders_brackets_and_body`.
pub(crate) fn build_rich_statusline(app: &ReplApp) -> Line<'static> {
    let prefix_spans = vec![Span::styled(
        "[trusty-agents] ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];

    let Some(status) = app.status_line.as_ref() else {
        // Legacy fallback: render the configured segment string dim.
        let text = render_statusline(app);
        let mut spans = prefix_spans;
        spans.push(Span::styled(
            text,
            Style::default().add_modifier(Modifier::DIM),
        ));
        return Line::from(spans);
    };

    // Re-style the well-known startup status string. The format is built in
    // `src/repl/mod.rs` as:
    //   "✓ LLM: {provider} ({model}) · All systems go."
    // We split on " · " so each chunk can be styled independently. Token
    // counts + estimated cost are injected dynamically from `app` (#293)
    // BEFORE the trailing `All systems go.` chunk, but only when the session
    // has accumulated tokens.
    let mut spans = prefix_spans;
    let raw_chunks: Vec<&str> = status.split(" · ").collect();
    // Build a vector of owned chunk strings with token/cost spliced in.
    let mut chunks: Vec<String> = Vec::with_capacity(raw_chunks.len() + 4);
    let has_tokens = app.tokens_in > 0 || app.tokens_out > 0;
    // #319: TM segment surfaces the live session count.
    let tm_chunk = format!(
        "TM: {} session{}",
        app.tm_session_count,
        if app.tm_session_count == 1 { "" } else { "s" }
    );
    // #319: local inference model segment — shown only when enabled+available.
    // Strip the "ollama/" vendor prefix so the display stays compact.
    let local_model_chunk: Option<String> = app.local_model.as_deref().map(|m| {
        let display = m.strip_prefix("ollama/").unwrap_or(m);
        format!("local: {display}")
    });
    let session_cost = crate::usage::daily::cost_from_tokens(app.tokens_in, app.tokens_out);
    let daily_cost = app.daily_cost_start + session_cost;
    // Show the daily-total segment only when there was prior usage today —
    // i.e. the "today" total exceeds the in-flight session cost. On a fresh
    // day, daily == session and the extra segment would be redundant.
    let show_daily = app.daily_cost_start > 0.0 && daily_cost > session_cost + 1e-9;
    for chunk in &raw_chunks {
        if has_tokens && chunk.starts_with("All systems go.") {
            chunks.push(format_token_chunk(app.tokens_in, app.tokens_out));
            if show_daily {
                chunks.push(format!("{} session", format_cost_value(session_cost)));
                chunks.push(format!("{} today", format_cost_value(daily_cost)));
            } else {
                chunks.push(format_cost_chunk(app.tokens_in, app.tokens_out));
            }
        }
        chunks.push((*chunk).to_string());
        // #319: insert the TM session count segment immediately after the
        // `LLM:` chunk so it sits at the high-signal end of the statusline.
        // Also insert the local model segment (when active) right after TM.
        if chunk.starts_with("✓ LLM:") {
            chunks.push(tm_chunk.clone());
            // #331: surface a distinct claude-mpm session count segment when
            // any TM session is running the claude-mpm adapter. Suppressed
            // when zero so the statusline doesn't carry empty noise.
            if app.claude_mpm_session_count > 0 {
                chunks.push(format!(
                    "style:claude_mpm MPM: {} session{}",
                    app.claude_mpm_session_count,
                    if app.claude_mpm_session_count == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
            }
            if let Some(ref lm) = local_model_chunk {
                chunks.push(lm.clone());
            }
        }
    }
    for (i, chunk) in chunks.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        spans.extend(style_status_chunk(chunk));
    }
    Line::from(spans)
}

/// Format the `↑1.2k ↓0.8k` token chunk for the statusline.
///
/// Why: Compact, two-arrow form mirrors Claude Code's status row.
/// What: Uses `format_tokens` (k-suffix when ≥1000) for both directions.
/// Test: `format_token_chunk_compacts_thousands`.
pub(crate) fn format_token_chunk(tokens_in: u64, tokens_out: u64) -> String {
    format!(
        "↑{} ↓{}",
        format_tokens(tokens_in),
        format_tokens(tokens_out)
    )
}

/// Format the `$0.0034` estimated-cost chunk for the statusline.
///
/// Why: Surfaces approximate spend at-a-glance using OpenRouter haiku
/// pricing (a reasonable default for the most-common harness model).
/// What: Cost = prompt_tokens * $0.00000025 + completion_tokens * $0.00000125.
/// Format with 4 decimals if <$0.01, 3 decimals otherwise.
/// Test: `format_cost_chunk_thresholds`.
pub(crate) fn format_cost_chunk(tokens_in: u64, tokens_out: u64) -> String {
    let cost = crate::usage::daily::cost_from_tokens(tokens_in, tokens_out);
    format_cost_value(cost)
}

/// Format a USD cost value with the same threshold rules as `format_cost_chunk`.
///
/// Why: Shared by the bare `$0.0034` chunk and the `$0.0034 session` /
/// `$0.0145 today` segments so the two never disagree on precision.
/// What: 4 decimals when cost < $0.01, 3 decimals otherwise.
/// Test: `format_cost_value_thresholds`.
pub(crate) fn format_cost_value(cost: f64) -> String {
    if cost < 0.01 {
        format!("${:.4}", cost)
    } else {
        format!("${:.3}", cost)
    }
}

/// Best-effort flush of the daily usage file, throttled to once per
/// `USAGE_WRITE_INTERVAL`.
///
/// Why: Token updates fire on every chunk; we don't want to fsync on each.
/// Throttling protects the disk while still surviving most crashes (worst
/// case: lose ≤5 s of in-flight cost).
/// What: Recomputes session cost from `tokens_in`/`tokens_out`, builds a
/// `DailyUsage` with today's date and `daily_cost_start + session_cost`,
/// then writes it atomically. I/O errors log at debug and are swallowed —
/// daily totals are observability, not control flow.
/// Test: `persist_daily_usage_writes_after_interval` (synchronous helper).
pub(crate) fn persist_daily_usage_if_due(app: &mut ReplApp) {
    let now = std::time::Instant::now();
    let due = match app.last_usage_write {
        None => true,
        Some(last) => now.duration_since(last) >= USAGE_WRITE_INTERVAL,
    };
    if !due {
        return;
    }
    let session_cost = crate::usage::daily::cost_from_tokens(app.tokens_in, app.tokens_out);
    let record = crate::usage::daily::DailyUsage {
        date: crate::usage::daily::today_local(),
        // Note: prompt_tokens / completion_tokens are *daily* totals on disk.
        // We don't carry forward prior session token counts (only their
        // cost), so this is best-effort: the cost line is the canonical
        // value, the token counts reflect this session only when no prior
        // session ran today. Good enough for the at-a-glance display.
        prompt_tokens: app.tokens_in,
        completion_tokens: app.tokens_out,
        cost_usd: app.daily_cost_start + session_cost,
    };
    if let Err(e) = crate::usage::daily::save_atomic(&app.usage_project_dir, &record) {
        tracing::debug!(error = %e, "daily usage: write failed");
    }
    app.last_usage_write = Some(now);
}

/// Style one ` · `-separated chunk of the startup status string.
///
/// Why: Each chunk has different semantics — the leading `✓ LLM: …` chunk
/// gets a green tick + bold model, the `Tools/Skills/MCP` chunks get dim
/// labels with normal counts, and the trailing `All systems go. Type /help …`
/// chunk gets green for the success message and dim for the hint.
/// What: Recognize chunk by prefix; emit a span vector. Unknown chunks pass
/// through dim.
/// Test: `rich_statusline_chunks_styled` covers each branch.
pub(crate) fn style_status_chunk(chunk: &str) -> Vec<Span<'static>> {
    // Leading `✓ LLM: provider (model)`.
    if let Some(rest) = chunk.strip_prefix("✓ LLM: ") {
        return vec![
            Span::styled("✓ ", Style::default().fg(Color::Green)),
            Span::raw("LLM: "),
            Span::styled(
                rest.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
    }
    // Trailing combined chunk: `All systems go.` (#293 dropped the
    // `Type /help for commands.` hint). Defensively handle either form.
    if let Some(rest) = chunk.strip_prefix("All systems go.") {
        return vec![
            Span::styled("All systems go.", Style::default().fg(Color::Green)),
            Span::styled(
                rest.to_string(),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ];
    }
    // Token/cost chunks (#293): `↑1.2k ↓0.8k` and `$0.003` render normal.
    if chunk.starts_with('↑') || chunk.starts_with('$') {
        return vec![Span::raw(chunk.to_string())];
    }
    // #319: TM session count segment — bold count, dim label.
    if let Some(rest) = chunk.strip_prefix("TM: ") {
        return vec![
            Span::styled("TM: ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                rest.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];
    }
    // #331: claude-mpm session count segment. The `style:claude_mpm ` sentinel
    // prefix is stripped here and the remainder rendered in bright magenta to
    // visually distinguish it from the dim/bold TM count.
    if let Some(rest) = chunk.strip_prefix("style:claude_mpm ") {
        // Split label ("MPM: ") from value ("N sessions") for hybrid styling.
        let (label, value) = match rest.find(": ") {
            Some(i) => (&rest[..i + 2], &rest[i + 2..]),
            None => ("", rest),
        };
        return vec![
            Span::styled(
                label.to_string(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                value.to_string(),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
    }
    // Unknown chunk — pass through dim.
    vec![Span::styled(
        chunk.to_string(),
        Style::default().add_modifier(Modifier::DIM),
    )]
}
