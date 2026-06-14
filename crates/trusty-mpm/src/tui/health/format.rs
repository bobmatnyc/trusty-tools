//! Pure string-formatting helpers for the health screen.
//!
//! Why: separating the byte/count/time formatters and the per-daemon panel-body
//! builders from the ratatui widgets lets every format be unit-tested without a
//! terminal backend and keeps the renderer terse.
//! What: the human-readable size / count / uptime / relative-time formatters,
//! the comma-grouping helper, the ASCII bar builder, and the
//! [`search_panel_lines`] / [`memory_panel_lines`] body builders (sharing the
//! private `panel_lines` template).
//! Test: `format_bytes_picks_unit`, `format_relative_time_handles_known_offsets`,
//! `search_panel_lines_format_fields`, `ascii_bar_fills_proportionally`.

use crate::tui::health::types::{PanelData, PanelState};

/// Format an RFC 3339 timestamp as a compact `[Xm/h/d ago]` badge.
///
/// Why: the collections list shows a freshness badge next to each row so the
/// operator can spot stale indexes; raw RFC 3339 strings are too wide for the
/// 28-column left panel.
/// What: parses the timestamp with `chrono::DateTime::parse_from_rfc3339`,
/// computes the signed delta against `Utc::now()`, and renders the largest
/// unit that yields a non-zero figure (`Xm`, `Xh`, or `Xd`). `None` or an
/// unparseable string yields `"never"`. A future timestamp (clock skew) is
/// reported as `"just now"`.
/// Test: `format_relative_time_handles_known_offsets`.
pub fn format_relative_time(ts: Option<&str>) -> String {
    let Some(s) = ts else {
        return "never".to_string();
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(s) else {
        return "never".to_string();
    };
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(parsed.with_timezone(&chrono::Utc));
    let secs = delta.num_seconds();
    if secs < 60 {
        // Includes negative (future) timestamps from clock skew.
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

/// Format a `uptime` in seconds as a compact `Xh Ym` string.
///
/// Why: the panel shows uptime; raw seconds are hard to read at a glance.
/// What: returns `"{hours}h {minutes}m"` — e.g. `3720` → `"1h 2m"`.
/// Test: `format_uptime_is_compact`.
pub fn format_uptime(secs: u64) -> String {
    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
}

/// Format a byte count as a human-readable size (`KB` / `MB` / `GB`).
///
/// Why: raw `disk_bytes` and `rss_mb` figures are easier to scan abbreviated.
/// What: returns one decimal place with a unit suffix, picking the largest
/// unit under which the value is `>= 1`.
/// Test: `format_bytes_picks_unit`.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}GB", b / GB)
    } else if b >= MB {
        format!("{:.1}MB", b / MB)
    } else if b >= KB {
        format!("{:.1}KB", b / KB)
    } else {
        format!("{bytes}B")
    }
}

/// Format an RSS figure (already in megabytes) as a human-readable size.
///
/// Why: `/health` reports RSS in whole megabytes; the panel shows it as `GB`
/// once it crosses 1024 MB so the figure stays short.
/// What: returns `"{x.x}GB"` above 1024 MB, otherwise `"{n}MB"`.
/// Test: `format_rss_picks_unit`.
pub fn format_rss(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1}GB", mb as f64 / 1024.0)
    } else {
        format!("{mb}MB")
    }
}

/// Format a large count compactly: exact below 10k, `Xk` above.
///
/// Why: chunk and vector counts run into the tens of thousands; an abbreviated
/// form keeps the fixed-width panel readable.
/// What: counts below 10,000 are shown exactly; larger counts as `{n}k` with
/// one decimal.
/// Test: `format_count_abbreviates_large`.
pub fn format_count(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Format a count with comma thousands separators (e.g. `1,234,567`).
///
/// Why: the detail panel surfaces graph stats that often run into the tens
/// of thousands; comma-grouping is easier to scan than a packed digit run.
/// What: walks the digits right-to-left and inserts a comma every three.
/// Test: `format_with_commas_groups_thousands`.
pub fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Build an ASCII bar of length `width`, filled to `ratio` (0.0..=1.0).
///
/// Why: ratatui's `Gauge` widget paints with colour; the spec asks for the
/// fixed-width `████████░░` glyph form rendered as text. A pure helper keeps
/// the proportion arithmetic unit-testable.
/// What: returns a string with `filled` blocks (`█`) then `width-filled`
/// dots (`░`).
/// Test: `ascii_bar_fills_proportionally`.
pub fn ascii_bar(ratio: f64, width: usize) -> String {
    let r = ratio.clamp(0.0, 1.0);
    let filled = (r * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// Build the text lines for the trusty-search panel body.
///
/// Why: separating line construction from the ratatui widgets lets a test
/// assert the rendered content without a terminal backend.
/// What: returns the panel body as plain strings — a header line with the
/// online indicator and version, then resource and count lines; an offline
/// panel shows its error, a connecting panel a placeholder.
/// Test: `search_panel_lines_format_fields`, `panel_lines_render_each_state`.
pub fn search_panel_lines(state: &PanelState, base_url: &str) -> Vec<String> {
    panel_lines(state, base_url, "SEARCH", |data| {
        vec![
            format!(
                "Indexes: {}  Chunks: {}",
                data.count_a,
                format_count(data.count_b)
            ),
            format!("Disk: {}", format_bytes(data.disk_bytes)),
        ]
    })
}

/// Build the text lines for the trusty-memory panel body.
///
/// Why: mirrors [`search_panel_lines`] for testable, terminal-free rendering.
/// What: returns the panel body as plain strings — header, resource lines, then
/// palace / vector / drawer / KG counts; offline and connecting states render
/// as for search.
/// Test: `memory_panel_lines_format_fields`, `panel_lines_render_each_state`.
pub fn memory_panel_lines(state: &PanelState, base_url: &str) -> Vec<String> {
    panel_lines(state, base_url, "MEMORY", |data| {
        vec![
            format!(
                "Palaces: {}  Vectors: {}",
                data.count_a,
                format_count(data.count_b)
            ),
            format!(
                "Drawers: {}  KG: {}",
                data.count_c,
                format_count(data.count_d)
            ),
        ]
    })
}

/// Shared panel-body builder for the search and memory panels.
///
/// Why: both panels share the header / resource / footer structure; only the
/// count lines differ, so they are supplied by the `counts` closure.
/// What: returns the header (indicator + version), the RSS / CPU / uptime line,
/// the daemon-specific count lines, a blank spacer, and the `[S]start [X]stop`
/// hint. Offline panels show the unreachable address, last error, and a retry
/// note; connecting panels show a placeholder.
/// Test: `panel_lines_render_each_state`.
fn panel_lines(
    state: &PanelState,
    base_url: &str,
    name: &str,
    counts: impl Fn(&PanelData) -> Vec<String>,
) -> Vec<String> {
    match state {
        PanelState::Connecting => vec![format!("{name} [○] connecting to {base_url}…")],
        PanelState::Offline { last_error } => vec![
            format!("{name} [○] OFFLINE"),
            format!("unreachable at {base_url}"),
            format!("last error: {last_error}"),
            "retrying every 5s…".to_string(),
            String::new(),
            "[S]start [X]stop".to_string(),
        ],
        PanelState::Online(data) => {
            let version = if data.version.is_empty() {
                "?".to_string()
            } else {
                format!("v{}", data.version)
            };
            let mut lines = vec![
                format!("{name} [●] {version}"),
                format!(
                    "RSS: {}  CPU: {:.0}%  Uptime: {}",
                    format_rss(data.rss_mb),
                    data.cpu_pct,
                    format_uptime(data.uptime_secs),
                ),
            ];
            lines.extend(counts(data));
            lines.push(String::new());
            lines.push("[S]start [X]stop".to_string());
            lines
        }
    }
}
