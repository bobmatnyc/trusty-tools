//! Pure welcome-panel renderer (no I/O).
//!
//! Why: keeping rendering side-effect-free makes it trivially unit-testable
//! and lets the print wrapper stay thin.
//! What: `render_welcome_panel` builds the `╭─╮` rounded box string from a
//! `WelcomeData` value; section helpers format the individual rows.
//! Test: `welcome_panel_renders_online`, `welcome_panel_renders_offline`,
//! `welcome_panel_renders_reconnecting`, `welcome_panel_shows_commits`,
//! `welcome_panel_omits_commits_when_empty`, `welcome_panel_truncates_subject`,
//! `welcome_panel_services_up_vs_down`, `welcome_panel_no_checkmark_on_not_detected`.

use unicode_width::UnicodeWidthStr as _;

use super::{ConsoleInfo, DaemonInfo, WelcomeData};
// LOGO / LOGO_COLS are only used by render_welcome_panel which is test-only.
#[cfg(test)]
use super::{LOGO, LOGO_COLS};

// ── Public render entry point ─────────────────────────────────────────────────

/// Render the welcome panel into a `String` (pure — no I/O, no probes).
///
/// Why: pure render is unit-testable; all probing and I/O happens in the
/// calling `gather_welcome_data` + `print_welcome_panel` functions.
/// What: builds a two-column `╭─╮` box — logo left, content right — with
/// sections for project/workspace, recent commits, service status, and a
/// TM commands cheat-sheet. Width adapts to the longest content line.
/// Test: `welcome_panel_renders_online`, `welcome_panel_renders_offline`,
/// `welcome_panel_renders_reconnecting`, `welcome_panel_shows_commits`.
/// Note: kept for test coverage of `build_rows`; production path uses the
/// two-panel compositor via `render_info_box_rows` + `banner::two_panel`.
#[cfg(test)]
pub(crate) fn render_welcome_panel(data: &WelcomeData) -> String {
    let rows = build_rows(data);
    let right_w = rows.iter().map(|r| display_len(r)).max().unwrap_or(30);

    // inner span = 1 space + logo + 1 space + divider + 1 space + content + 1 space
    let inner = 1 + LOGO_COLS + 1 + 1 + 1 + right_w + 1;
    let top = format!("\u{256d}{}\u{256e}", "\u{2500}".repeat(inner));
    let bot = format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(inner));

    let blank_logo = " ".repeat(LOGO_COLS);
    let mut out = String::new();
    out.push_str(&top);
    out.push('\n');
    for (i, row) in rows.iter().enumerate() {
        let logo_line: &str = LOGO.get(i).copied().unwrap_or(&blank_logo);
        let padded = pad_to(row, right_w);
        out.push_str(&format!(
            "\u{2502} {logo_line} \u{2502} {padded} \u{2502}\n"
        ));
    }
    out.push_str(&bot);
    out.push('\n');
    out
}

// ── Row builder ───────────────────────────────────────────────────────────────

/// Build all right-column rows for the welcome panel.
///
/// Why: separating row construction from box-drawing keeps each part small
/// and independently testable. Also consumed by the two-panel compositor
/// (`banner::two_panel`) to populate the right panel without re-implementing
/// the row-building logic.
/// What: returns rows in display order: header, project/workspace, optional
/// reconnecting, optional commit section, service section (console first since
/// #6761, then daemon, memory, search, review), commands section.
/// Test: covered by `render_welcome_panel` tests; `two_panel_compose_alignment`;
/// `welcome_panel_renders_the_console_row_with_its_port`.
pub(crate) fn build_rows(data: &WelcomeData) -> Vec<String> {
    let version = env!("CARGO_PKG_VERSION");
    let workspace = super::abbreviate_home(&data.workspace);

    let mut rows: Vec<String> = Vec::new();

    // ── Header ───────────────────────────────────────────────────────────────
    rows.push(format!("trusty-mpm v{version}"));
    if data.user.is_empty() {
        rows.push(String::new());
    } else {
        rows.push(format!("Welcome back {}!", data.user));
    }
    rows.push(String::new());

    // ── Project / workspace ───────────────────────────────────────────────────
    rows.push(format!("project    {}", data.project));
    rows.push(format!("workspace  {workspace}"));
    if data.reconnecting && !data.session_name.is_empty() {
        rows.push(format!(
            "session    \u{21a9} reconnecting  ({})",
            data.session_name
        ));
    }

    // ── Recent commits ────────────────────────────────────────────────────────
    if !data.recent_commits.is_empty() {
        rows.push(String::new());
        rows.push("recent changes".to_string());
        for c in &data.recent_commits {
            let entry = format!("  {}  \u{2022}  {}  \u{2022}  {}", c.sha, c.age, c.subject);
            rows.push(truncate_display(entry, 60));
        }
    }

    // ── Services ─────────────────────────────────────────────────────────────
    rows.push(String::new());
    rows.push("services".to_string());
    // #6761: the console is what an operator opens in a browser, so it leads.
    rows.push(format_console_row(&data.console));
    rows.push(format_daemon_row(&data.daemon));
    rows.push(format_service_row("memory", &data.memory_status));
    rows.push(format_service_row("search", &data.search_status));
    rows.push(format_service_row("review", &data.review_status));

    // ── Commands cheat-sheet ──────────────────────────────────────────────────
    rows.push(String::new());
    rows.push("commands".to_string());
    rows.push("  tm sessions list     tm sessions resume <id>".to_string());
    rows.push("  tm connect            tm stop".to_string());
    rows.push("  tm status             detach: ^b d".to_string());

    rows
}

// ── Row formatters ────────────────────────────────────────────────────────────

/// Format the console status row.
///
/// Why (#6761): the operator opens the console in a browser, so this row prints
/// the port — the one place in the block where an address is the point. It is
/// the console's OWN port, read from its discovery record, and never the
/// daemon's, which #6869 removed from the row below and which this row must not
/// reintroduce under a new label.
/// What: `"  \u{25cf} console  :7788"` when the probe answered, using the port
/// half of [`ConsoleInfo::addr`]; `"  \u{25cb} console  offline"` when it did
/// not, and also when the address carries no port to show — an offline row
/// names no address at all.
/// Test: `welcome_panel_renders_the_console_row_with_its_port`,
/// `welcome_panel_console_row_offline_names_no_port`,
/// `welcome_panel_never_contains_the_daemon_port`.
fn format_console_row(console: &ConsoleInfo) -> String {
    let port = console
        .addr
        .rsplit_once(':')
        .map(|(_, port)| port)
        .filter(|port| console.online && !port.is_empty());
    match port {
        Some(port) => format!("  \u{25cf} console  :{port}"),
        None => "  \u{25cb} console  offline".to_string(),
    }
}

/// Format the daemon status row.
///
/// Why: the daemon row has two forms (online/offline) with an optional session
/// count; a helper keeps `build_rows` readable. It names no port and no
/// transport (#6869): the port it used to print was whatever the discovery
/// chain happened to resolve, and it will be wrong outright once the daemon
/// moves to a Unix socket (#6288). `tm doctor` (#6336) and `tm statusline`
/// (#6304) already say only whether the daemon answers; this row now matches.
/// What: `"  \u{25cf} daemon  running  (2)"` when online; `"  \u{25cb} daemon  offline"` when not.
/// Test: `welcome_panel_renders_online`, `welcome_panel_renders_offline`,
/// `welcome_panel_never_contains_the_daemon_port`.
fn format_daemon_row(daemon: &DaemonInfo) -> String {
    if daemon.online {
        // #6869: the operator gets liveness and session count, never the port.
        let count_str = match daemon.session_count {
            Some(n) => format!("  ({n})"),
            None => String::new(),
        };
        format!("  \u{25cf} daemon  running{count_str}")
    } else {
        "  \u{25cb} daemon  offline".to_string()
    }
}

/// Format a named service row (memory / search / review).
///
/// Why: detected services get a `✓`, undetected get `○`; a helper keeps
/// `build_rows` DRY.
/// What: `"  \u{2713} <name>  <value>"` when detected; `"  \u{25cb} <name>  not detected"` when not.
/// Test: `welcome_panel_services_up_vs_down`,
/// `welcome_panel_no_checkmark_on_not_detected`.
fn format_service_row(name: &str, status: &str) -> String {
    if status == "(not detected)" {
        format!("  \u{25cb} {name:<7}  not detected")
    } else {
        format!("  \u{2713} {name:<7}  {status}")
    }
}

// ── String helpers ────────────────────────────────────────────────────────────

/// Measure the terminal display width of `s` in columns.
///
/// Why: `str::len()` counts bytes; `.chars().count()` counts codepoints — both
/// undercount CJK / wide characters (e.g. `'修'` renders as 2 columns). Without
/// the correct column count the closing `│` border misaligns and
/// `truncate_display` cuts one column short per wide char.
/// What: delegates to `unicode_width::UnicodeWidthStr::width()` which follows
/// Unicode Standard Annex #11 (East Asian Width), returning 2 for wide chars
/// and 1 for narrow/ASCII chars.
/// Test: `display_len_ascii_equals_char_count`,
/// `display_len_cjk_wider_than_char_count`,
/// `welcome_panel_cjk_commit_box_alignment`.
pub(crate) fn display_len(s: &str) -> usize {
    s.width()
}

/// Pad `s` with trailing spaces to `width` display columns.
///
/// Why: the right column requires uniform width so the closing `│` aligns.
/// What: appends `(width - display_len(s))` spaces; returns `s` unchanged
/// when already ≥ `width` columns.
/// Test: indirectly by render tests.
/// Note: only called from `render_welcome_panel` which is test-only.
#[cfg(test)]
pub(crate) fn pad_to(s: &str, width: usize) -> String {
    let len = display_len(s);
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

/// Truncate `s` to at most `max_cols` display columns, appending `…`.
///
/// Why: commit subjects can be very long; truncation keeps the panel width
/// sane. Using `chars().take(n)` is wrong for wide (CJK) characters — each
/// takes 2 columns but only 1 codepoint. This function accumulates column
/// widths correctly.
/// What: iterates chars, accumulating `unicode_width` column counts. Stops
/// before exceeding `max_cols - 1` columns (1 reserved for `…`), then appends
/// `…` (U+2026). Returns `s` unchanged when it already fits within `max_cols`.
/// Test: `welcome_panel_truncates_subject`,
/// `truncate_display_cjk_respects_column_budget`.
fn truncate_display(s: String, max_cols: usize) -> String {
    if display_len(&s) <= max_cols {
        return s;
    }
    // Budget: max_cols - 1 columns for text, 1 for the ellipsis.
    let budget = max_cols.saturating_sub(1);
    let mut cols_used: usize = 0;
    let mut truncated = String::new();
    for ch in s.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if cols_used + ch_width > budget {
            break;
        }
        cols_used += ch_width;
        truncated.push(ch);
    }
    format!("{truncated}\u{2026}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formatters::info_box::CommitLine;

    fn online_daemon(port: u16) -> DaemonInfo {
        DaemonInfo {
            addr: format!("127.0.0.1:{port}"),
            online: true,
            session_count: None,
        }
    }

    fn offline_daemon() -> DaemonInfo {
        DaemonInfo::default()
    }

    fn base_data() -> WelcomeData {
        WelcomeData {
            project: "owner/repo".to_string(),
            workspace: "/home/user/projects/repo".to_string(),
            user: "alice".to_string(),
            reconnecting: false,
            session_name: String::new(),
            daemon: offline_daemon(),
            console: ConsoleInfo::default(),
            recent_commits: vec![],
            memory_status: "(not detected)".to_string(),
            search_status: "(not detected)".to_string(),
            review_status: "(not detected)".to_string(),
        }
    }

    #[test]
    fn welcome_panel_renders_online() {
        let data = WelcomeData {
            daemon: online_daemon(7880).with_count(2),
            ..base_data()
        };
        let out = render_welcome_panel(&data);
        assert!(out.contains('\u{25cf}'), "expected ● online marker");
        // #6869: liveness, not an address.
        assert!(out.contains("daemon  running"), "expected running label");
        assert!(out.contains("(2)"), "expected session count");
        assert!(out.contains("owner/repo"), "expected project");
        assert!(out.contains("alice"), "expected user name");
    }

    /// Why (#6869): the welcome banner rendered `● daemon  :7880`, so bare `tm`
    /// put the daemon's port in front of every operator on every launch. The
    /// port is an implementation detail of a transport that is moving to a Unix
    /// socket (#6288), and `tm doctor` (#6336) and `tm statusline` (#6304)
    /// already hide it. Mirrors `render_statusline_never_contains_the_daemon_port`.
    /// What: renders the online row through both the standalone panel and the
    /// production two-panel row builder, with a non-default port so the
    /// assertion cannot pass by coincidence against a hardcoded 7880, and
    /// checks that neither the port nor the loopback host survives.
    /// Test: this is the test.
    #[test]
    fn welcome_panel_never_contains_the_daemon_port() {
        for port in [7880u16, 9911] {
            let data = WelcomeData {
                daemon: online_daemon(port).with_count(2),
                ..base_data()
            };
            let panel = render_welcome_panel(&data);
            let rows = crate::formatters::info_box::render_info_box_rows(&data).join("\n");
            for (label, out) in [("panel", &panel), ("two-panel rows", &rows)] {
                assert!(
                    !out.contains(&port.to_string()),
                    "{label}: the daemon port must not reach the operator: {out}"
                );
                assert!(
                    !out.contains("127.0.0.1"),
                    "{label}: the daemon host must not reach the operator: {out}"
                );
                assert!(
                    out.contains("daemon  running"),
                    "{label}: liveness must still be reported: {out}"
                );
                assert!(out.contains("(2)"), "{label}: session count kept: {out}");
            }
        }
        // The offline form has nothing to leak, and must stay that way.
        let offline = render_welcome_panel(&base_data());
        assert!(
            !offline.contains("127.0.0.1") && !offline.contains("7880"),
            "offline row must name no address: {offline}"
        );
    }

    /// Why (#6761): owner ruling 2026-09-03 — the services block must show the
    /// console and the port an operator types into a browser, not only the
    /// daemon. The port is taken from the probed address, so a console moved
    /// off its default is still reachable from what the banner prints.
    /// What: renders through both the standalone panel and the production
    /// two-panel row builder, with a non-default port so the assertion cannot
    /// pass against a hardcoded 7788, and pins the console row ahead of the
    /// daemon row.
    /// Test: this is the test.
    #[test]
    fn welcome_panel_renders_the_console_row_with_its_port() {
        let data = WelcomeData {
            console: ConsoleInfo {
                addr: "127.0.0.1:9312".to_string(),
                online: true,
            },
            ..base_data()
        };
        let panel = render_welcome_panel(&data);
        let rows = crate::formatters::info_box::render_info_box_rows(&data);
        assert!(
            panel.contains("console  :9312"),
            "the console row must name its own port: {panel}"
        );
        for (label, out) in [("panel", &panel), ("two-panel rows", &rows.join("\n"))] {
            let console_at = out.find("console").expect("console row");
            let daemon_at = out.find("daemon").expect("daemon row");
            assert!(
                console_at < daemon_at,
                "{label}: the console row renders first: {out}"
            );
        }
    }

    /// Why (#6761): the row is probed, so "console" must never imply "reachable".
    /// What: the offline form carries the `○` glyph, the word `offline`, and no
    /// address at all — including when the probe answered but the resolved
    /// address carries no port to print.
    /// Test: this is the test.
    #[test]
    fn welcome_panel_console_row_offline_names_no_port() {
        for console in [
            ConsoleInfo {
                addr: "127.0.0.1:9312".to_string(),
                online: false,
            },
            ConsoleInfo {
                addr: "localhost".to_string(),
                online: true,
            },
            ConsoleInfo::default(),
        ] {
            let row = format_console_row(&console);
            assert_eq!(row, "  \u{25cb} console  offline", "{console:?}");
        }
    }

    #[test]
    fn welcome_panel_renders_offline() {
        let out = render_welcome_panel(&base_data());
        assert!(out.contains('\u{25cb}'), "expected ○ marker somewhere");
        assert!(out.contains("offline"), "expected offline for daemon");
    }

    #[test]
    fn welcome_panel_renders_reconnecting() {
        let data = WelcomeData {
            reconnecting: true,
            session_name: "tmpm-my-proj".to_string(),
            ..base_data()
        };
        let out = render_welcome_panel(&data);
        assert!(out.contains("reconnecting"), "expected reconnecting text");
        assert!(out.contains("tmpm-my-proj"), "expected session name");
    }

    #[test]
    fn welcome_panel_shows_commits() {
        let data = WelcomeData {
            recent_commits: vec![
                CommitLine {
                    sha: "abc1234".to_string(),
                    age: "2h".to_string(),
                    subject: "fix: something important".to_string(),
                },
                CommitLine {
                    sha: "def5678".to_string(),
                    age: "1d".to_string(),
                    subject: "feat: add new feature".to_string(),
                },
            ],
            ..base_data()
        };
        let out = render_welcome_panel(&data);
        assert!(out.contains("recent changes"), "expected section header");
        assert!(out.contains("abc1234"), "expected first sha");
        assert!(out.contains("2h"), "expected first age");
        assert!(out.contains("fix: something"), "expected first subject");
        assert!(out.contains("def5678"), "expected second sha");
    }

    #[test]
    fn welcome_panel_omits_commits_when_empty() {
        let out = render_welcome_panel(&base_data());
        assert!(
            !out.contains("recent changes"),
            "section must not appear when commits is empty"
        );
    }

    #[test]
    fn welcome_panel_truncates_subject() {
        // A very long subject should be truncated.
        let long_subject = "a".repeat(100);
        let data = WelcomeData {
            recent_commits: vec![CommitLine {
                sha: "abc1234".to_string(),
                age: "1h".to_string(),
                subject: long_subject,
            }],
            ..base_data()
        };
        let out = render_welcome_panel(&data);
        // Each line in the rendered box must fit in a reasonable width.
        for line in out.lines() {
            assert!(
                line.chars().count() <= 200,
                "line too long (>200 cols): {line:?}"
            );
        }
        assert!(
            out.contains('\u{2026}'),
            "expected ellipsis on truncated subject"
        );
    }

    #[test]
    fn welcome_panel_services_up_vs_down() {
        let data = WelcomeData {
            memory_status: "trusty-memory".to_string(),
            search_status: "(not detected)".to_string(),
            review_status: "(not detected)".to_string(),
            ..base_data()
        };
        let out = render_welcome_panel(&data);
        // memory detected → ✓
        let memory_line = out
            .lines()
            .find(|l| l.contains("memory"))
            .expect("memory line must exist");
        assert!(
            memory_line.contains('\u{2713}'),
            "detected memory must show ✓: {memory_line:?}"
        );
        // search not detected → ○
        let search_line = out
            .lines()
            .find(|l| l.contains("search"))
            .expect("search line must exist");
        assert!(
            search_line.contains('\u{25cb}'),
            "undetected search must show ○: {search_line:?}"
        );
    }

    #[test]
    fn welcome_panel_no_checkmark_on_not_detected() {
        let out = render_welcome_panel(&base_data());
        for line in out.lines() {
            if line.contains("not detected") {
                assert!(
                    !line.contains('\u{2713}'),
                    "✓ must not appear on a not-detected line: {line:?}"
                );
            }
        }
    }

    #[test]
    fn welcome_panel_contains_commands_cheatsheet() {
        let out = render_welcome_panel(&base_data());
        assert!(
            out.contains("tm sessions list"),
            "expected sessions list command"
        );
        assert!(out.contains("tm connect"), "expected connect command");
        assert!(out.contains("tm stop"), "expected stop command");
        assert!(out.contains("^b d"), "expected detach hint");
    }

    #[test]
    fn welcome_panel_no_user_graceful() {
        let data = WelcomeData {
            user: String::new(),
            ..base_data()
        };
        // Must not panic; "Welcome back" should not appear without a user name.
        let out = render_welcome_panel(&data);
        assert!(out.contains("trusty-mpm"), "box must still render");
        assert!(
            !out.contains("Welcome back"),
            "must omit greeting when user is empty"
        );
    }

    #[test]
    fn display_len_ascii_equals_char_count() {
        // For plain ASCII, display width == char count.
        let s = "hello world";
        assert_eq!(display_len(s), s.chars().count());
    }

    #[test]
    fn display_len_cjk_wider_than_char_count() {
        // Each CJK character is 1 codepoint but 2 display columns.
        // "修复" = 2 chars, 4 display columns.
        let s = "修复";
        assert_eq!(s.chars().count(), 2, "codepoint count");
        assert_eq!(display_len(s), 4, "display width must be 4 for 2 CJK chars");
    }

    #[test]
    fn truncate_display_short_string_unchanged() {
        let s = "hello".to_string();
        assert_eq!(truncate_display(s.clone(), 80), s);
    }

    #[test]
    fn truncate_display_long_string_gets_ellipsis() {
        let s = "a".repeat(70);
        let result = truncate_display(s, 60);
        assert!(result.ends_with('\u{2026}'), "must end with ellipsis");
        assert!(display_len(&result) <= 60, "must be at most 60 cols");
    }

    #[test]
    fn truncate_display_cjk_respects_column_budget() {
        // "feat: " = 6 ascii cols, then 27 CJK chars × 2 cols = 54 cols.
        // Total = 60 cols. max_cols = 20.
        // Budget = 19 cols: "feat: " (6) + 6 CJK × 2 (12) = 18 cols, then ellipsis.
        let s = format!("feat: {}", "修".repeat(27));
        assert!(display_len(&s) > 20, "sanity: input is wide");
        let result = truncate_display(s, 20);
        assert!(
            result.ends_with('\u{2026}'),
            "must end with ellipsis: {result:?}"
        );
        assert!(
            display_len(&result) <= 20,
            "truncated result must fit in 20 cols, got {} cols: {result:?}",
            display_len(&result)
        );
    }

    #[test]
    fn welcome_panel_cjk_commit_box_alignment() {
        // A commit with a CJK subject must not break the box geometry.
        let data = WelcomeData {
            recent_commits: vec![CommitLine {
                sha: "abc1234".to_string(),
                age: "1h".to_string(),
                subject: "feat: 修复登录问题并优化性能".to_string(),
            }],
            ..base_data()
        };
        let out = render_welcome_panel(&data);
        // Every line in the box must start with `│` and end with `│`.
        for line in out.lines() {
            if line.starts_with('\u{2502}') {
                assert!(
                    line.ends_with('\u{2502}'),
                    "box border must close with │: {line:?}"
                );
            }
        }
        // The commit sha must appear in the output.
        assert!(out.contains("abc1234"), "sha must be present");
    }

    /// Smoke test: render and print the panel with representative data.
    ///
    /// Why: visual regression check — lets a developer run with `--nocapture`
    /// to see the actual rendered panel before shipping.
    /// What: builds `WelcomeData` with sample commits, online daemon, and a
    /// detected service, then prints the rendered panel to stdout.
    /// Test: run with `cargo test -p trusty-mpm welcome_panel_smoke -- --nocapture`.
    #[test]
    fn welcome_panel_smoke_render() {
        let data = WelcomeData {
            project: "duetto-research/trusty-tools".to_string(),
            workspace: "/Users/masa/Projects/trusty-tools".to_string(),
            user: "masa".to_string(),
            reconnecting: false,
            session_name: String::new(),
            daemon: DaemonInfo {
                addr: "127.0.0.1:7880".to_string(),
                online: true,
                session_count: Some(2),
            },
            // #6761: the services block gained a console row.
            console: ConsoleInfo {
                addr: "127.0.0.1:7788".to_string(),
                online: true,
            },
            recent_commits: vec![
                CommitLine {
                    sha: "83f30ba".to_string(),
                    age: "2h".to_string(),
                    subject: "fix: guided resume restarts a stopped session".to_string(),
                },
                CommitLine {
                    sha: "8c7665b".to_string(),
                    age: "6h".to_string(),
                    subject: "fix: decouple AppState build from MCP initialize".to_string(),
                },
                CommitLine {
                    sha: "db0a115".to_string(),
                    age: "1d".to_string(),
                    subject: "feat: tm welcome banner box + rich statusline".to_string(),
                },
            ],
            memory_status: "trusty-memory".to_string(),
            search_status: "trusty-search".to_string(),
            review_status: "(not detected)".to_string(),
        };
        let out = render_welcome_panel(&data);
        println!("\n{out}");
        // Structural assertions
        assert!(out.contains("trusty-mpm"), "must contain branding");
        assert!(out.contains("Welcome back masa!"), "must contain greeting");
        assert!(out.contains("83f30ba"), "must contain first sha");
        assert!(out.contains("trusty-memory"), "must show memory detected");
        assert!(out.contains("commands"), "must contain commands section");
    }
}
