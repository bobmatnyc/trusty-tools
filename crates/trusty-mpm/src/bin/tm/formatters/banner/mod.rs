//! Launch-banner rendering for `tm launch` and `tm connect`.
//!
//! Why: the full-screen ASCII-art banner is ~130 lines of data and ~80 lines
//! of rendering logic; keeping it separate from the launch handler keeps that
//! file under the 500-line cap and makes it trivially testable in isolation.
//! What: `print_launch_banner`, `print_launch_banner_reconnecting`,
//! `print_banner_preview`, `terminal_width`, `detect_memory`, `detect_tool`,
//! `dirs_config_dir`, `binary_on_path`, `fallback_session_name`,
//! `normalize_workdir`, `tmux_has_session`. The single-box compositor lives in
//! the `two_panel` submodule. Banner loading lives in `source`. Auto-sized
//! shading lives in `shade_image` / `load_and_shade_banner`.
//! Test: `launch_banner_*`, `terminal_width_is_positive`,
//! `normalize_workdir_strips_trailing_slash` in `tests.rs`;
//! `two_panel_*` in `two_panel::tests`; `banner_source_*` in `source::tests`.

pub(crate) mod source;
pub(crate) mod two_panel;

use colored::Colorize as _;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr as _;

// ── RAII color-disable guard ──────────────────────────────────────────────────

/// RAII guard that disables `colored` output for the duration of a scope.
///
/// Why: calling `colored::control::set_override(false)` then `unset_override()`
/// manually is not panic-safe — a panic between the two leaves global color state
/// permanently disabled for all subsequent tests or output. A Drop guard restores
/// the override unconditionally even when the scope exits via panic or early return.
/// What: on construction calls `set_override(false)` when `active` is true;
/// on `drop` calls `unset_override()`. When `active` is false, no-ops both.
/// Test: `no_color_guard_restores_on_drop`.
pub(crate) struct NoColorGuard {
    active: bool,
}

impl NoColorGuard {
    /// Create a guard that disables color when `disable` is `true`.
    ///
    /// Why: callers detect non-TTY and pass `!is_tty`; this keeps the
    /// call site clean (`let _g = NoColorGuard::new(!is_tty)`).
    /// What: sets `colored::control::set_override(false)` immediately when
    /// `disable` is true; otherwise constructs a no-op guard.
    /// Test: `no_color_guard_restores_on_drop`.
    pub(crate) fn new(disable: bool) -> Self {
        if disable {
            colored::control::set_override(false);
        }
        Self { active: disable }
    }
}

impl Drop for NoColorGuard {
    fn drop(&mut self) {
        if self.active {
            colored::control::unset_override();
        }
    }
}

// ── Per-character rust brightness shading ─────────────────────────────────────

/// Map a glyph to its rust-palette RGB triple.
///
/// Why: bot outline chars should appear bright amber so the block robots read
/// clearly against a dark terminal background; accent glyphs use mid-rust to
/// add depth without competing with the main structure. The v0.12.0 splash
/// (issue #1907) renders everything — robots and the `«Trusty»` wordmark — in
/// a single amber/orange tone against black, so the wordmark's letters and
/// guillemets join the same bright-amber bucket as the robot glyphs rather
/// than falling through to the darker "unclassified" default.
/// What: five buckets —
///   • amber `(224,140,60)`: block, half-block, and face glyphs used in the
///     block-robot art (`▄ ▀ █ ▓ ▌ ▐ ▟ ▙`), box-drawing chars, face glyphs
///     (`◉ ◔ ◕ • ◡ ▿ ⌣ ● ◻ ^ ⢀ ✲ ⡀`) from current and legacy art, and the
///     `«Trusty»` wordmark glyphs (`T r u s t y « »`);
///   • mid-rust `(205,100,30)`: medium-ink marks;
///   • dark rust `(120,50,10)`: fine punctuation marks;
///   • base rust `(183,65,14)`: everything else.
/// Test: `image_shading_emits_truecolor` in `two_panel::tests`.
pub(crate) fn shade_bucket(c: char) -> (u8, u8, u8) {
    match c {
        // Amber — block structure: half-block, full-block, shade chars
        '▄' | '▀' | '█' | '▓' | '▌' | '▐' | '▟' | '▙'
        // Box-drawing chars (legacy kawaii bots)
        | '┌' | '─' | '┐' | '│' | '└' | '┘' | '┬' | '┴' | '├' | '┤' | '╷' | '╵' | '╶' | '╴'
        // Face glyphs — block robots and legacy
        | '◉' | '◔' | '◕' | '•' | '◡' | '▿' | '⌣'
        | '^' | '●' | '⢀' | '✲' | '⡀' | '◻'
        // Legacy dense glyphs from previous art
        | 'I' | '∏' | '♦' | '∇' | '√' | '≥' | '≤' | '@' | '#'
        // «Trusty» wordmark (#1907): letters + guillemets
        | 'T' | 'r' | 'u' | 's' | 't' | 'y' | '«' | '»' => {
            (224, 140, 60) // bright amber
        }
        // Medium — moderate ink
        'i' | 'l' | '!' | '<' | '>' | '+' | '=' | '/' | '\\' | '≈' | '∫' | '∑' => {
            (205, 100, 30) // mid rust
        }
        // Light — fine punctuation marks
        '.' | ',' | ':' | ';' | '°' | '⋆' | '◦' | '~' | '-' => {
            (120, 50, 10) // dark rust
        }
        // Unclassified — base rust
        _ => (183, 65, 14),
    }
}

/// Shade an art string and return `(coloured_rows, max_display_cols)`.
///
/// Why: auto-computing the display width from the actual art means an
/// arbitrarily-sized user-edited file renders correctly without recompiling;
/// all previous hard-coded CLIP_* constants are replaced by this measurement.
/// Taking `use_color` as an explicit parameter (issue #1858) rather than
/// probing the process-global `colored::control` override internally makes
/// this function pure and deterministic: tests can exercise both the
/// color-on and color-off paths directly, with no shared mutable state to
/// race against other tests running in parallel threads.
/// What: iterates every line of `art`, colourises non-space chars via
/// `shade_bucket` when `use_color` is `true`, pads each row to
/// `max_display_cols` with spaces. Returns `(rows, max_display_cols)`. Spaces
/// are transparent (no colour escape) regardless of `use_color`. Rows wider
/// than `max_display_cols` are clipped gracefully (no panic).
/// Test: `image_autosize_uses_art_dimensions`, `image_clip_correct_cols`,
/// `image_shading_emits_truecolor` in `two_panel::tests`.
pub(crate) fn shade_image(art: &str, use_color: bool) -> (Vec<String>, usize) {
    let raw_lines: Vec<&str> = art.lines().collect();
    let max_cols = raw_lines.iter().map(|l| l.width()).max().unwrap_or(0);

    if max_cols == 0 || raw_lines.is_empty() {
        return (Vec::new(), 0);
    }

    let rows = raw_lines
        .iter()
        .map(|line| {
            let mut row = String::with_capacity(max_cols * 24);
            let mut display_cols: usize = 0;

            for c in line.chars() {
                let cw = UnicodeWidthChar::width(c).unwrap_or(1);
                if display_cols + cw > max_cols {
                    break; // clip gracefully — don't panic
                }
                if c == ' ' {
                    row.push(' ');
                } else if use_color {
                    let (r, g, b) = shade_bucket(c);
                    row.push_str(&format!("\x1B[38;2;{r};{g};{b}m{c}\x1B[0m"));
                } else {
                    row.push(c);
                }
                display_cols += cw;
            }
            // Pad to max_cols.
            for _ in display_cols..max_cols {
                row.push(' ');
            }
            row
        })
        .collect();

    (rows, max_cols)
}

/// Load the banner art from disk (or embedded default) and shade it.
///
/// Why: decouples the renderer (`two_panel`) from the I/O of discovering and
/// reading the user-editable banner file. This is the one legitimate
/// production read of `colored`'s global override: a real launch must follow
/// whatever `NoColorGuard`/TTY detection has configured for the process, so
/// the probe happens here, once, and is threaded into the now-pure
/// `shade_image` rather than that function reading the global itself.
/// What: calls `source::load_banner_art()`, probes `colored`'s current
/// override via a throwaway `truecolor()` call, then calls
/// `shade_image(&art, use_color)`, returning `(coloured_rows,
/// max_display_cols)`.
/// Test: `image_autosize_uses_art_dimensions` in `two_panel::tests`.
pub(crate) fn load_and_shade_banner() -> (Vec<String>, usize) {
    let art = source::load_banner_art();
    // Probe colored's current control state so our raw truecolor escapes respect
    // the same set_override(false) call that banner callers use for non-TTY mode.
    // When set_override(false) is active, truecolor() returns bare text and the
    // probe string has no ANSI escape — emitting raw escapes would bypass that.
    let use_color = "a".truecolor(0, 0, 0).to_string().contains('\x1B');
    shade_image(&art, use_color)
}

/// Query the terminal width in columns, falling back to 80 when unknown.
///
/// Why: the launch banner clears the screen and the caller may want the width
/// for future centering; a robust width probe avoids panics on pipes/CI.
/// What: issues a `TIOCGWINSZ` ioctl on stdout, then falls back to the
/// `$COLUMNS` environment variable, then to a hard-coded 80.
/// Test: `terminal_width_is_positive` asserts the result is always > 0.
pub(crate) fn terminal_width() -> usize {
    // SAFETY: `winsize` is a plain-old-data struct; `ioctl` only writes into it
    // and we check the return code before reading the result.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut ws) == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(n) = cols.parse::<usize>()
        && n > 0
    {
        return n;
    }
    80
}

/// Print the full-screen `tm launch` banner (single outer box).
///
/// Why: `tm launch` should give the operator a readable splash screen before
/// the terminal is taken over by `claude`/`tmux`. The single-box banner clears
/// the screen for visual impact; the info panel provides rich operational context.
/// What: clears the screen, then renders the single-box layout via
/// `two_panel::render_two_panel_banner`. Pauses 1 s after rendering.
/// Test: `launch_banner_does_not_panic`.
pub(crate) fn print_launch_banner(
    workdir: &str,
    tmux_name: &str,
    _prompt_path: Option<&std::path::Path>,
    managed_path: Option<&std::path::Path>,
) {
    let w = terminal_width();
    let daemon = super::info_box::DaemonInfo::from_lock_file_with_probe();
    let data =
        super::info_box::gather_welcome_data(workdir, tmux_name, false, daemon, managed_path);

    if let Some(panel) = two_panel::render_two_panel_banner(&data, w, false) {
        println!("\x1B[2J\x1B[1;1H");
        print!("{panel}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::thread::sleep(std::time::Duration::from_secs(1));
    } else {
        // Extremely narrow terminal (< 40 cols) — very rare.
        println!("\x1B[2J\x1B[1;1H");
        println!("Trusty MPM v{}", env!("CARGO_PKG_VERSION"));
        println!("Launching...");
    }
}

/// Print the full-screen `tm launch` banner with a "reconnecting" status line.
///
/// Why: when `tm launch` attaches to a pre-existing session the operator should
/// see that no new session was created.
/// What: on terminals >= MIN_WIDTH_BOX renders the single-box banner with
/// reconnecting state shown. On very narrow terminals falls back to plain text.
/// Test: `launch_reconnect_banner_does_not_panic`.
pub(crate) fn print_launch_banner_reconnecting(workdir: &str, tmux_name: &str) {
    let w = terminal_width();
    let daemon = super::info_box::DaemonInfo::from_lock_file_with_probe();
    let data = super::info_box::gather_welcome_data(workdir, tmux_name, true, daemon, None);

    if let Some(panel) = two_panel::render_two_panel_banner(&data, w, true) {
        println!("\x1B[2J\x1B[1;1H");
        print!("{panel}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::thread::sleep(std::time::Duration::from_secs(1));
    } else {
        println!("\x1B[2J\x1B[1;1H");
        println!("Trusty MPM v{}", env!("CARGO_PKG_VERSION"));
        println!("Reconnecting...");
    }
}

/// Print the two-panel daily banner for the no-subcommand `tm` guided flow (#1808).
///
/// Why: the daily `tm` banner should visually match `tm banner` — version in the
/// title bar (not as a separate content row), 24-row clipped art, and
/// project/workspace fields — instead of the compact 3-row LOGO via
/// `render_welcome_panel`. No screen-clear and no sleep since the session picker
/// follows immediately below. Non-TTY callers are already gated out by `tty_gate`
/// before this is invoked.
/// What: builds `WelcomeData` from `workdir` (derives github project + workspace),
/// renders via `render_two_panel_banner`, and prints to stdout. On very narrow
/// terminals (<MIN_WIDTH_BOX cols) prints a one-line plain-text fallback.
/// Test: `daily_banner_two_panel_version_in_title_bar` in `tests.rs`.
pub(crate) fn print_daily_banner(workdir: &str, daemon: &super::info_box::DaemonInfo) {
    use std::io::IsTerminal as _;
    let is_tty = std::io::stdout().is_terminal();
    // RAII guard: restores color state on return OR on panic — not panic-safe without it.
    let _color_guard = NoColorGuard::new(!is_tty);
    let w = terminal_width();
    // Rebuild DaemonInfo by value (no Clone on purpose — same pattern as print_info_box).
    let d = super::info_box::DaemonInfo {
        addr: daemon.addr.clone(),
        online: daemon.online,
        session_count: daemon.session_count,
    };
    let data = super::info_box::gather_welcome_data(workdir, "", false, d, None);
    if let Some(panel) = two_panel::render_two_panel_banner(&data, w, false) {
        print!("\n{panel}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    } else {
        println!("Trusty MPM v{}", env!("CARGO_PKG_VERSION"));
    }
}

/// Print the banner to stdout for preview — no screen-clear, no sleep.
///
/// Why: `tm banner` lets the operator eyeball the colored image + welcome panel
/// without launching Claude or waiting a full second. Skipping the
/// `\x1B[2J\x1B[1;1H` clear preserves the operator's scrollback, and the
/// absence of the 1-second sleep makes iteration fast.
/// What: on terminals >= MIN_WIDTH_BOX renders the single-box banner (no
/// screen-clear). On very narrow terminals prints a minimal plain-text banner.
/// Test: `banner_preview_does_not_panic` in `tests.rs`.
pub(crate) fn print_banner_preview(reconnecting: bool) {
    use std::io::IsTerminal as _;
    let is_tty = std::io::stdout().is_terminal();
    // RAII guard: restores color state on return OR on panic — not panic-safe without it.
    let _color_guard = NoColorGuard::new(!is_tty);
    let w = terminal_width();
    let workdir = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let session_name = fallback_session_name(std::path::Path::new(&workdir));
    let daemon = super::info_box::DaemonInfo::from_lock_file_with_probe();
    let data =
        super::info_box::gather_welcome_data(&workdir, &session_name, reconnecting, daemon, None);

    if let Some(panel) = two_panel::render_two_panel_banner(&data, w, reconnecting) {
        print!("\n{panel}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    } else {
        println!("Trusty MPM v{}", env!("CARGO_PKG_VERSION"));
        if reconnecting {
            println!("Reconnecting...");
        }
    }
}

/// Detect whether the `trusty-memory` MCP integration is available.
///
/// Why: the launch banner reports which trusty companions are wired up.
/// What: returns `"trusty-memory"` when `~/.config/trusty-memory` exists or the
/// `trusty-memory` binary is on `PATH`, else `"(not detected)"`.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
pub(crate) fn detect_memory() -> String {
    let config = dirs_config_dir().map(|c| c.join("trusty-memory"));
    let has_config = config.map(|c| c.exists()).unwrap_or(false);
    if has_config || binary_on_path("trusty-memory") {
        "trusty-memory".to_string()
    } else {
        "(not detected)".to_string()
    }
}

/// Detect whether a named tool binary is available on `PATH`.
///
/// Why: the launch banner reports `trusty-search` availability.
/// What: returns the tool name when its binary is on `PATH`, else
/// `"(not detected)"`.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
pub(crate) fn detect_tool(name: &str) -> String {
    if binary_on_path(name) {
        name.to_string()
    } else {
        "(not detected)".to_string()
    }
}

/// Return the user's config directory (`~/.config` on Linux/macOS).
///
/// Why: `detect_memory` probes `~/.config/trusty-memory` without pulling in a
/// platform-dirs dependency.
/// What: returns `$XDG_CONFIG_HOME` when set, else `$HOME/.config`.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
pub(crate) fn dirs_config_dir() -> Option<std::path::PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(std::path::PathBuf::from(xdg));
    }
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
}

/// Check whether an executable named `name` exists on `PATH`.
///
/// Why: banner detection of `trusty-memory` / `trusty-search` needs a
/// dependency-free `which`-style lookup.
/// What: scans each `PATH` entry for an existing `name` file.
/// Test: covered indirectly by `launch_banner_does_not_panic`.
pub(crate) fn binary_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

/// Compute the fallback `tmpm-<folder>` session name for a project directory.
///
/// Why: when the daemon is unreachable `tm launch` still needs a tmux session
/// name; deriving it from the project folder keeps the offline name identical
/// to the one the daemon would assign for the same directory.
/// What: returns `name_from_dir(path)` (`tmpm-<sanitized-folder>`).
/// Test: `fallback_session_name_has_tmpm_prefix`,
/// `fallback_session_name_uses_folder`.
pub(crate) fn fallback_session_name(path: &std::path::Path) -> String {
    trusty_mpm::core::names::name_from_dir(path)
}

/// Normalize a working-directory path for equality comparison.
///
/// Why: two paths can name the same directory yet differ textually (trailing
/// slash, relative vs absolute, symlinks); `tm launch` reconnect detection must
/// treat them as equal.
/// What: canonicalizes the path when it exists, otherwise strips a trailing
/// slash from the lossy string form.
/// Test: `normalize_workdir_strips_trailing_slash`.
pub(crate) fn normalize_workdir(workdir: &str) -> String {
    let path = std::path::Path::new(workdir);
    if let Ok(canonical) = path.canonicalize() {
        return canonical.to_string_lossy().to_string();
    }
    workdir.trim_end_matches('/').to_string()
}

/// Check whether a tmux session named `name` currently exists.
///
/// Why: the daemon may hold a stale session record after its tmux session has
/// exited; `tm launch` must verify the tmux session is live before attaching,
/// otherwise it would fall through to a normal launch.
/// What: runs `tmux has-session -t <name>` and returns true on exit code 0.
/// Test: covered indirectly by the launch reconnect integration path.
pub(crate) fn tmux_has_session(name: &str) -> bool {
    matches!(
        std::process::Command::new("tmux")
            .args(["has-session", "-t", name])
            .status(),
        Ok(status) if status.success()
    )
}
