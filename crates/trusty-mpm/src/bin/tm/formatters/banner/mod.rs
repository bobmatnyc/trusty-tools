//! Launch-banner rendering for `tm launch` and `tm connect`.
//!
//! Why: the full-screen ASCII-art banner is ~130 lines of data and ~80 lines
//! of rendering logic; keeping it separate from the launch handler keeps that
//! file under the 500-line cap and makes it trivially testable in isolation.
//! What: `print_launch_banner`, `print_launch_banner_reconnecting`,
//! `print_banner_preview`, `terminal_width`, `detect_memory`, `detect_tool`,
//! `dirs_config_dir`, `binary_on_path`, `fallback_session_name`,
//! `normalize_workdir`, `tmux_has_session`. The single-box compositor lives in
//! the `two_panel` submodule. Image clipping and rust-shading live here.
//! Test: `launch_banner_*`, `terminal_width_is_positive`,
//! `normalize_workdir_strips_trailing_slash` in `tests.rs`;
//! `two_panel_*` in `two_panel::tests`.

pub(crate) mod two_panel;

// ── Embedded ASCII-art image ──────────────────────────────────────────────────

/// Raw ASCII-art image embedded at compile time.
///
/// Why: shipping the art as a sidecar `.txt` keeps the source file readable
/// and avoids escaping every special Unicode glyph in a Rust string literal.
/// What: 16-line × 52-col kawaii row-of-three pixel-bot artwork; clipped to
/// the focal figure window by `clip_and_shade_image`.
/// Test: `image_clip_correct_rows` in `two_panel::tests`.
pub(crate) const BANNER_IMAGE: &str = include_str!("image.txt");

// ── Clip-window constants ─────────────────────────────────────────────────────

/// First row to include (0-indexed, inclusive).
pub(crate) const CLIP_ROW_START: usize = 0;
/// One-past-the-last row to include (0-indexed, exclusive).
/// Set to 16 to capture the full kawaii row-of-three bot art.
pub(crate) const CLIP_ROW_END: usize = 16;
/// First column to include (0-indexed, inclusive).
pub(crate) const CLIP_COL_START: usize = 0;
/// One-past-the-last column to include (0-indexed, exclusive).
pub(crate) const CLIP_COL_END: usize = 52;
/// Display width of the clipped image in columns.
pub(crate) const IMAGE_CLIP_COLS: usize = CLIP_COL_END - CLIP_COL_START;
/// Number of rows in the clipped image.
pub(crate) const IMAGE_CLIP_ROWS: usize = CLIP_ROW_END - CLIP_ROW_START;

// ── Per-character rust brightness shading ─────────────────────────────────────

/// Map a glyph to its rust-palette RGB triple.
///
/// Why: bot outline chars should appear bright amber so the three kawaii bots
/// read clearly against a dark terminal background; accent glyphs use mid-rust
/// to add depth without competing with the main structure.
/// What: four buckets —
///   • amber `(224,140,60)`: box-drawing, block, face, and lightbulb glyphs
///     used in the kawaii bot art (`┌─┐│└┘┬╷▟▙^●◡⢀✲⡀◻`) plus legacy dense
///     glyphs from the old art;
///   • mid-rust `(205,100,30)`: medium-ink marks;
///   • dark rust `(120,50,10)`: fine marks;
///   • base rust `(183,65,14)`: everything else.
/// Test: `image_shading_emits_truecolor` in `two_panel::tests`.
pub(crate) fn shade_bucket(c: char) -> (u8, u8, u8) {
    match c {
        // Amber — bot structure: box-drawing, block chars, face glyphs, lightbulb
        '┌' | '─' | '┐' | '│' | '└' | '┘' | '┬' | '┴' | '├' | '┤' | '╷' | '╵'
        | '▟' | '▙' | '▌' | '▐' | '█' | '▓'
        | '^' | '●' | '◡' | '⢀' | '✲' | '⡀' | '◻'
        // Legacy dense glyphs from previous art
        | 'I' | '∏' | '♦' | '∇' | '√' | '≥' | '≤' | '@' | '#' => {
            (224, 140, 60) // bright amber
        }
        // Medium — moderate ink
        'i' | 'l' | '!' | '<' | '>' | '+' | '=' | '/' | '\\' | '≈' | '∫' | '∑' => {
            (205, 100, 30) // mid rust
        }
        // Light — fine marks
        '.' | ',' | ':' | ';' | '°' | '⋆' | '•' | '◦' | '~' | '-' => {
            (120, 50, 10) // dark rust
        }
        // Unclassified — base rust
        _ => (183, 65, 14),
    }
}

/// Clip the ASCII-art image to the focal window and apply per-char rust shading.
///
/// Why: the kawaii bot art is a compact 16-row × 52-col region; the clip
/// window captures it at native character resolution without downsampling.
/// What: slices rows `[CLIP_ROW_START, CLIP_ROW_END)` and chars
/// `[CLIP_COL_START, CLIP_COL_END)` from `BANNER_IMAGE`; applies
/// `shade_bucket` per non-space character; pads short lines to `IMAGE_CLIP_COLS`.
/// Spaces are passed through uncoloured (transparent).  Each row is already
/// coloured via truecolor escapes; consumers measure display width via
/// `strip_ansi` + `unicode_width`.
/// Test: `image_clip_correct_rows`, `image_clip_correct_cols`,
/// `image_shading_emits_truecolor` in `two_panel::tests`.
pub(crate) fn clip_and_shade_image() -> Vec<String> {
    BANNER_IMAGE
        .lines()
        .skip(CLIP_ROW_START)
        .take(IMAGE_CLIP_ROWS)
        .map(|line| {
            let chars: Vec<char> = line.chars().collect();
            let len = chars.len();
            let col_start = CLIP_COL_START; // 0 — no min needed
            let col_end = CLIP_COL_END.min(len);

            let mut row = String::with_capacity(IMAGE_CLIP_COLS * 24);
            let mut display_cols: usize = 0;

            for &c in &chars[col_start..col_end] {
                if c == ' ' {
                    row.push(' ');
                } else {
                    // Emit raw truecolor escape directly rather than via the
                    // `colored` crate so the output is stable regardless of the
                    // global `colored` override state (which is shared across
                    // parallel tests and can race). strip_ansi in two_panel.rs
                    // handles `\x1B[…m` sequences correctly for width math.
                    let (r, g, b) = shade_bucket(c);
                    row.push_str(&format!("\x1B[38;2;{r};{g};{b}m{c}\x1B[0m"));
                }
                display_cols += 1;
            }
            // Pad with spaces to reach IMAGE_CLIP_COLS.
            for _ in display_cols..IMAGE_CLIP_COLS {
                row.push(' ');
            }
            row
        })
        .collect()
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
    let daemon = super::info_box::DaemonInfo::from_lock_file();
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
    let daemon = super::info_box::DaemonInfo::from_lock_file();
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
    let w = terminal_width();
    let workdir = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let session_name = fallback_session_name(std::path::Path::new(&workdir));
    let daemon = super::info_box::DaemonInfo::from_lock_file();
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
