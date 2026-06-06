//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Strip the leading `vendor/` prefix from a model id for compact display.
///
/// Why: The TUI activity row and statusline previously rendered the full id
/// (`anthropic/claude-haiku-4-5`) which wastes horizontal space and reads as
/// noise — users already know the provider from the statusline label.
/// What: Returns everything after the first `/`. If the input has no `/`,
/// returns it unchanged. Pure, allocation-light (`String` only when slicing).
/// Test: `strip_vendor_prefix_*` unit tests below.
pub(crate) fn strip_vendor_prefix(model: &str) -> String {
    match model.find('/') {
        Some(i) => model[i + 1..].to_string(),
        None => model.to_string(),
    }
}

/// True when a thinking-step line is redundant with row 1's cycling status word.
///
/// Why: `draw_activity` would otherwise show "thinking…" on both the spinner
/// row (row 1) and the latest-thinking row (row 3) when the LLM hasn't yet
/// emitted any meaningful step text. Suppressing the dup keeps row 3 free
/// for real progress signals when they arrive.
/// What: Lower-cases the step, trims whitespace and trailing `.` / `…`
/// characters, then matches against the three status words emitted by
/// `status_word_for`. Empty string is also considered redundant.
/// Test: `is_redundant_thinking_step_*` unit tests below.
pub(crate) fn is_redundant_thinking_step(step: &str) -> bool {
    let trimmed: String = step
        .trim()
        .trim_end_matches(['.', '…'])
        .trim()
        .to_ascii_lowercase();
    matches!(trimmed.as_str(), "" | "thinking" | "working" | "processing")
}

/// Format an elapsed-seconds value as `Xs`, `Xm Ys`, or `Xh Ym`.
///
/// Why: Claude Code's spinner shows `(2m 18s · ...)` style elapsed; we mirror
/// it so users don't decode large `1234s` figures.
/// What: <60s → `Ns`; <3600s → `Mm Ss`; otherwise → `Hh Mm`.
/// Test: `format_elapsed_buckets`.
pub(crate) fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Format a token count compactly: `<1000` raw, `>=1000` as `1.2k`.
///
/// Why: Claude Code spinner uses `↓ 2.9k tokens` — keep it short.
/// What: Round to one decimal at the `k` boundary; trim trailing `.0` to
/// keep `2k` instead of `2.0k`.
/// Test: `format_tokens_compact`.
pub(crate) fn format_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = (n as f64) / 1000.0;
    let s = format!("{:.1}k", k);
    // Trim `.0k` → `k` for a tighter render.
    if let Some(stripped) = s.strip_suffix(".0k") {
        format!("{}k", stripped)
    } else {
        s
    }
}

/// Pick the cycling status word from elapsed seconds.
///
/// Why: Claude Code rotates "thinking / working / processing"; we bucket on
/// elapsed time so the word changes deterministically as the call grows.
/// What: 0–9s = thinking, 10–29s = working, 30s+ = processing.
/// Test: `status_word_buckets`.
pub(crate) fn status_word_for(elapsed_secs: u64) -> &'static str {
    match elapsed_secs {
        0..=9 => "thinking",
        10..=29 => "working",
        _ => "processing",
    }
}

/// Build the spinner row 1 of the activity panel.
///
/// Why: Pulled out so we can unit-test the composition (glyph, elapsed,
/// token segment, status word) without a Terminal.
/// What: `✻ Processing… (Xs · ↓ Yk tokens · word)` — `✻` in yellow, rest dim.
/// Token segment is omitted when both `tokens_in` and `tokens_out` are zero.
/// Test: `activity_row1_includes_elapsed_and_status`,
/// `activity_row1_omits_tokens_when_zero`.
pub(crate) fn build_activity_row1(
    app: &ReplApp,
    elapsed_secs: u64,
    max_width: usize,
) -> Line<'static> {
    let elapsed = format_elapsed(elapsed_secs);
    let word = status_word_for(elapsed_secs);

    let mut paren_parts: Vec<String> = Vec::with_capacity(3);
    paren_parts.push(elapsed);
    if app.tokens_in > 0 || app.tokens_out > 0 {
        // While streaming we usually only have completion tokens (the prompt
        // is not yet billed). Show `↓ N tokens` if no prompt tokens yet,
        // otherwise the full `↑ N ↓ N tokens` pair.
        let tok_seg = if app.tokens_in == 0 {
            format!("↓ {} tokens", format_tokens(app.tokens_out))
        } else {
            format!(
                "↑ {} ↓ {} tokens",
                format_tokens(app.tokens_in),
                format_tokens(app.tokens_out)
            )
        };
        paren_parts.push(tok_seg);
    }
    paren_parts.push(word.to_string());

    let body = format!("Processing… ({})", paren_parts.join(" · "));
    // Animated braille spinner cycled by `app.tick_count` (~10fps from the
    // event-loop tick). Frames mirror Claude Code's spinner so the activity
    // strip reads as "actively working" instead of a frozen glyph.
    let glyph_char = SPINNER_FRAMES[(app.tick_count as usize) % SPINNER_FRAMES.len()];
    let glyph = format!("{} ", glyph_char);
    let combined = format!("{}{}", glyph, body);
    let truncated = truncate_to(combined, max_width);
    // Apply the Rust-rainbow flow effect across every character of the
    // activity row while busy. The activity panel only renders when busy
    // (see `draw` layout in this module), so this code path implies busy.
    Line::from(rainbow_spans(&truncated, app.rainbow_tick))
}

/// Spinner frames cycled by `tick_count`. Braille pattern matches Claude Code's
/// spinner — visually distinct from the static `✻` and reads smoothly at 10fps.
pub(crate) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// All slash commands surfaced by `/help`, used for inline autocomplete.
///
/// Why: As the user types `/` the inline picker filters this list by prefix,
/// letting them arrow-pick or Tab-complete a command instead of remembering
/// the exact name. Centralizing the list here keeps autocomplete in lockstep
/// with `write_help()` in `src/repl/mod.rs` — when a new command is added,
/// both must be updated.
/// What: `(name, short_description)` pairs for all 22 user-facing slash
/// commands. Names include the leading `/`. Description is shown alongside
/// in the future; current picker only renders the name.
/// Test: `slash_completions_filters_by_prefix`,
/// `slash_completions_clears_on_space`, `slash_completions_tab_completes`.
pub(crate) const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "show help"),
    ("/clear", "clear chat"),
    ("/exit", "quit"),
    ("/status", "system status"),
    ("/model", "select LLM model"),
    ("/provider", "select LLM provider"),
    ("/agent", "run a specific agent"),
    ("/switch", "switch persona"),
    ("/agents", "list agents"),
    ("/skills", "list skills"),
    ("/memories", "show memories"),
    ("/session", "session info"),
    ("/connect", "connect to project"),
    ("/version", "show version"),
    ("/projects", "list projects"),
    ("/log", "toggle log"),
    ("/run", "run workflow"),
    ("/history", "show history"),
    ("/telegram", "telegram bot control"),
    ("/logs", "show recent logs"),
    ("/local", "local inference control"),
    ("/tm", "tmux session manager"),
    ("/service", "manage persistent daemon"),
    ("/update", "check for and install updates"),
];

/// Recompute `app.choices` from the current input buffer to drive the inline
/// slash-command autocomplete picker.
///
/// Why: As the user types `/` the inline picker should narrow to commands that
/// match the typed prefix, then disappear once a space is typed (the command
/// name is locked in and the user is now typing arguments). Reusing the
/// existing `app.choices` plumbing means we get rendering, arrow navigation,
/// and Enter-to-insert for free — no new picker widget needed.
/// What: When `input_buf` starts with `/` and contains no space, populate
/// `choices` with matching command names (descriptions discarded for now to
/// keep the inserted text clean). Otherwise clear `choices`. Suppresses the
/// picker on an exact single-match (e.g. user typed `/help` fully) so the
/// picker doesn't visually echo what they already typed.
/// Test: `slash_completions_filters_by_prefix`,
/// `slash_completions_clears_on_space`,
/// `slash_completions_suppressed_on_exact_match`.
pub(crate) fn update_slash_completions(app: &mut ReplApp) {
    // Don't clobber an active context-driven picker (e.g. `/switch` persona
    // list). Those have a non-`None` context tag and a different lifecycle.
    if app.choices_context.is_some() {
        return;
    }
    let buf = &app.input_buf;
    if buf.starts_with('/') && !buf.contains(' ') {
        let prefix = buf.to_lowercase();
        let matches: Vec<String> = SLASH_COMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(prefix.as_str()))
            .map(|(cmd, _)| (*cmd).to_string())
            .collect();
        // Suppress when there's exactly one match and it equals what the
        // user already typed — no value showing the picker.
        let is_exact_single = matches.len() == 1 && matches[0] == *buf;
        if !matches.is_empty() && !is_exact_single {
            app.choices = matches;
            app.choice_cursor = 0;
            app.choices_context = None;
        } else {
            app.choices.clear();
            app.choice_cursor = 0;
        }
    } else {
        // Only clear if the choices we're holding are slash-command picks
        // (i.e. all start with '/'). Don't stomp on LLM-offered choice lists.
        let all_slash = !app.choices.is_empty() && app.choices.iter().all(|c| c.starts_with('/'));
        if all_slash {
            app.choices.clear();
            app.choice_cursor = 0;
        }
    }
}

/// Convert HSL (h: 0.0–360.0, s: 0.0–1.0, l: 0.0–1.0) to RGB (0–255 each).
///
/// Why: The flowing shimmer effect uses a continuous hue band rather than a
/// discrete palette, which requires HSL → RGB conversion at render time.
/// What: Standard HSL→RGB algorithm; returns the linear RGB triple.
/// Test: `hsl_to_rgb_edge_cases` covers white, black, and pure red.
pub(crate) fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h1 = h / 60.0;
    let x = c * (1.0 - (h1 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = if h1 < 1.0 {
        (c, x, 0.0)
    } else if h1 < 2.0 {
        (x, c, 0.0)
    } else if h1 < 3.0 {
        (0.0, c, x)
    } else if h1 < 4.0 {
        (0.0, x, c)
    } else if h1 < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

/// Animated horizontal HSL gradient across `text`, slowly drifting with `tick`.
///
/// Why: A discrete palette jumps between colors; the target effect (Claude
/// Code's spinner shimmer) is a smooth horizontal hue gradient that slowly
/// rotates over time. Equivalent to a CSS `linear-gradient` with `hue-rotate`
/// animation. Stays in the warm rust → orange → amber band.
/// What: Spreads chars across a 35° hue band starting at 5° (deep rust-red),
/// shifted by `tick * 0.8°` for the temporal drift. At ~10 ticks/sec this is
/// 8°/sec — a full 360° cycle every ~45s, matching the slow Claude Code drift.
/// One `Span` per char so each glyph carries its own color.
/// Test: `rainbow_spans_advances_with_tick`,
/// `rainbow_spans_one_span_per_char`, `hsl_to_rgb_edge_cases`.
pub(crate) fn rainbow_spans(text: &str, tick: usize) -> Vec<Span<'static>> {
    let len = text.chars().count().max(1);
    // Hue band: 5° (deep rust-red) → 40° (amber). Width = 35°.
    const BASE_HUE: f32 = 5.0;
    const BAND: f32 = 35.0;
    // Each tick shifts the gradient by 0.8° (full cycle ~450 ticks ≈ 45s at
    // 100ms/tick). The visible band sweeps through in ~4s.
    const DRIFT_PER_TICK: f32 = 0.8;
    let time_offset = (tick as f32 * DRIFT_PER_TICK) % 360.0;

    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let pos = i as f32 / len as f32;
            let hue = (BASE_HUE + pos * BAND + time_offset) % 360.0;
            let (r, g, b) = hsl_to_rgb(hue, 0.90, 0.55);
            Span::styled(c.to_string(), Style::default().fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

/// Parse an explicit `↓ N tokens` / `↓N tokens` count from a thinking-step
/// string (#298).
///
/// Why: Some upstream emitters surface partial completion-token counts in the
/// step text (e.g. `↓ 2.4k tokens`). When present, that's a more accurate
/// signal than the per-step estimate, so the TUI should snap to it.
/// What: Looks for a `↓` glyph followed by an optional space, a number with
/// optional `k`/`K` suffix (1k = 1000), and the literal `tokens` token. Returns
/// `None` if no match. Pure / allocation-light.
/// Test: `parse_token_count_from_step_*` unit tests below.
pub(crate) fn parse_token_count_from_step(s: &str) -> Option<u64> {
    let idx = s.find('↓')?;
    let rest = &s[idx + '↓'.len_utf8()..];
    let rest = rest.trim_start();
    // Read digits, optional `.` digits, optional k/K.
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let num_str = &rest[..i];
    let num: f64 = num_str.parse().ok()?;
    let after = &rest[i..];
    let (multiplier, after) = if let Some(stripped) = after.strip_prefix(['k', 'K']) {
        (1000.0, stripped)
    } else {
        (1.0, after)
    };
    // Require the word "tokens" follows (with optional whitespace) so we
    // don't false-match `↓ 5 lines` or similar.
    if !after.trim_start().starts_with("token") {
        return None;
    }
    Some((num * multiplier).round() as u64)
}

/// Truncate a `String` to `max_chars` characters (chars, not bytes), appending
/// nothing — this is a pure cap.
pub(crate) fn truncate_to(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    s.chars().take(max_chars).collect()
}

/// Strip leading and trailing whitespace-only lines from a multi-line string.
///
/// Why: LLM responses regularly include extra `\n\n` at the head or tail
/// (especially when the model emits a paragraph break before/after a closing
/// emoji). Rendering them verbatim produces visible blank rows in the chat
/// scrollback. Trimming once at the boundary keeps interior blank lines
/// (which carry meaning) intact.
/// What: Returns a String with all leading whitespace-only lines and all
/// trailing whitespace removed. Interior lines, including blank paragraph
/// breaks, are preserved.
/// Test: `trim_surrounding_blank_lines_*` unit tests.
/// Drop ALL whitespace-only lines from within a response.
///
/// Why: LLM responses arrive with markdown-style double-newline paragraph
/// breaks. In a terminal chat panel those blank rows accumulate into wasted
/// vertical space — the user reads consecutive paragraphs as a single
/// flowing thought, so the gaps just push later content off-screen. Removing
/// every interior blank produces a tight, compact response block.
/// What: Returns a String where every whitespace-only line is dropped.
/// Non-blank lines are preserved verbatim and joined with single `\n`s.
/// Test: `strip_interior_blank_lines_*` unit tests.
pub(crate) fn strip_interior_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(line);
        first = false;
    }
    out
}

/// Drop runs of 2+ consecutive whitespace-only `Line`s, keeping at most one.
///
/// Why: Same motivation as `collapse_inner_blank_lines` but operates on the
/// already-spanned `Vec<Line>` produced by `draw_chat` so paragraph gaps the
/// renderer itself inserted (e.g. the trailing blank after every assistant
/// response) don't compound when adjacent.
/// What: Returns a new `Vec<Line>` with consecutive whitespace-only lines
/// collapsed to a single blank `Line`.
/// Test: `collapse_blank_lines_*` unit tests.
pub(crate) fn collapse_blank_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
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

pub(crate) fn trim_surrounding_blank_lines(s: &str) -> String {
    // First trim trailing whitespace (covers tabs, spaces, and newlines).
    let trimmed_end = s.trim_end();
    // Then drop leading whitespace-only lines.
    let mut start = 0usize;
    let bytes = trimmed_end.as_bytes();
    while start < bytes.len() {
        // Find next newline.
        let nl = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p);
        let line_end = nl.unwrap_or(bytes.len());
        let line = &trimmed_end[start..line_end];
        if line.trim().is_empty() {
            // Skip the blank line including the newline.
            start = match nl {
                Some(p) => p + 1,
                None => bytes.len(),
            };
        } else {
            break;
        }
    }
    trimmed_end[start..].to_string()
}

/// Render a dim horizontal rule across the given area.
///
/// Why: Acts as the visual "bar above the input" that #290 inadvertently
/// removed when it stripped the input's bordered block. Keeps the modern
/// borderless look while restoring the demarcation users rely on to find
/// the prompt at a glance.
/// What: A single `─` repeated across the row, dim-styled. Renders nothing
/// if `area.height == 0`.
/// Test: Visual via `scripts/tmux-repl-test.sh`; geometry is mechanical.
pub(crate) fn draw_separator(f: &mut ratatui::Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let rule: String = "─".repeat(area.width as usize);
    let p = Paragraph::new(rule).style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(p, area);
}

/// Build a centered popup `Rect` covering `percent_x` × `percent_y` of `r`.
///
/// Why: ratatui has no built-in helper; this is the canonical idiom from the
/// official examples. Used by `draw_picker` to position the overlay.
/// What: Vertical split → take middle band → horizontal split → take middle column.
/// Test: Geometry is exercised visually; correctness is mechanical.
pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
