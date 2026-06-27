//! Launch-banner rendering for `tm launch` and `tm connect`.
//!
//! Why: the full-screen ASCII-art banner is ~130 lines of data and ~100 lines
//! of rendering logic; keeping it separate from the launch handler keeps that
//! file under the 500-line cap and makes it trivially testable in isolation.
//! What: `render_launch_banner`, `print_launch_banner`,
//! `print_launch_banner_reconnecting`, `terminal_width`, `detect_memory`,
//! `detect_tool`, `dirs_config_dir`, `binary_on_path`, `fallback_session_name`,
//! `normalize_workdir`, `tmux_has_session`. The two-panel compositor lives in
//! the `two_panel` submodule.
//! Test: `launch_banner_*`, `terminal_width_is_positive`,
//! `normalize_workdir_strips_trailing_slash` in `tests.rs`;
//! `two_panel_*` in `two_panel::tests`.

pub(crate) mod two_panel;

use colored::Colorize as _;

/// Left indent applied to every line of the full-screen launch banner.
pub(crate) const BANNER_INDENT: &str = "   ";

/// Width of the session-info separator line drawn in the launch banner.
#[allow(dead_code)]
pub(crate) const BANNER_SEPARATOR_WIDTH: usize = 53;

/// The ASCII-art robot mascot drawn at the top of the launch banner.
///
/// Why: a recognizable centerpiece gives `tm launch` the same "the tool has
/// taken over the terminal" feel as claude-mpm's startup screen.
/// What: a multi-line string-art robot; each line is printed verbatim with the
/// shared [`BANNER_INDENT`].
pub(crate) const BANNER_ROBOT: &[&str] = &[
    "                                             ",
    "                    .}##-                    ",
    "                    }#+#}                    ",
    "                   .}#-#}.                   ",
    "               ^]}#}##+##}#}]^               ",
    "            ^}}]<^++]#<#]++^^<}}<            ",
    "          <}]^++++++<#}#<++---+^]}]          ",
    "        ^}]+--++++++<###<+---+---+<}^        ",
    "       <}<+---++---+<###<-+--------^}<       ",
    "      -}<+-+^<<^+---+}##+---+^^<+---^}+      ",
    "     .]}++<}}<<]}]+-+}##--+]}]<<]}<--]].     ",
    "   ^#}#]^]].     ^#^-}##-^#<      ]]+]#}#^   ",
    "   }}]#]<#+       ]]+}#}-]}       -#<]#<}}   ",
    " ]##}<#]^}^      .}<-<#<.<}-      ^}^<#<}##] ",
    "<}^}}<#]-^}}-   <#<--<#<--<#<   -}}^-<}^}}+}<",
    "-#]}}^#]---^]##}^---.^#^-..-^}##]+..-]#^}}]#-",
    "  +#}^#]-----.----....+..............<#^]#+  ",
    "   }}^#]-----..--....................<}^}}   ",
    "   }}<#]--...--........   ........ ..<#^}}   ",
    "   ]}<#]-....--.........    .........<#^}]   ",
    "   ^}}#]^^++--------------.-...---++^]#]}^   ",
    "    }}]]}}}}##}}}}}}}}}}}}}}}}}##}}]]]]}}    ",
    "    }}+-----<#+     .   .     -#<-.----}]    ",
    "    -#<-----<}+  ..           -}<-...-<#-    ",
    "     .}}<+--<#+..             -}<-.-^}}.     ",
    "        ^}#}##}}}}}}}}}}}}}}}}}##}#}^        ",
    "                                             ",
];

/// Amber rust tone used for the plain-text wordmark label.
///
/// Why: reuses the warm amber from the bottom of the robot gradient so the
/// wordmark reads as part of the same brand palette without repeating the full
/// gradient computation.
/// What: RGB values `(224, 140, 60)` — the last entry in `ROBOT_GRADIENT`.
/// Test: visual inspection via `tm banner`.
const WORDMARK_R: u8 = 224;
const WORDMARK_G: u8 = 140;
const WORDMARK_B: u8 = 60;

/// Muted dark-rust tone for the version line beneath the wordmark.
///
/// Why: the version number should be present but not compete with the label.
/// What: RGB values `(140, 60, 20)` — darker than the wordmark amber.
/// Test: visual inspection via `tm banner`.
const VERSION_R: u8 = 140;
const VERSION_G: u8 = 60;
const VERSION_B: u8 = 20;

/// Build the two-line plain-text wordmark: `trusty` (bold amber) + version (dimmed rust).
///
/// Why: replaces the bulky 7-row block-art `BANNER_TITLE` with a single-line
/// label that is lighter, faster to scan, and stays under the SLOC cap.
/// What: returns a `[String; 2]` — `lines[0]` is the colourised `"trusty"` label,
/// `lines[1]` is the colourised `"v{CARGO_PKG_VERSION}"` version string. Both
/// degrade gracefully to plain text when `colored` colour is disabled.
/// Test: `wordmark_lines_contain_trusty_and_version`.
pub(crate) fn wordmark_lines() -> [String; 2] {
    use colored::Colorize as _;
    let label = "trusty"
        .truecolor(WORDMARK_R, WORDMARK_G, WORDMARK_B)
        .bold()
        .to_string();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"))
        .truecolor(VERSION_R, VERSION_G, VERSION_B)
        .to_string();
    [label, version]
}

/// Rust-color gradient palette applied row-by-row across the robot art.
///
/// Why: a smooth gradient from dark rust (top) through burnt orange (middle)
/// to amber (lower) turns the monochrome ASCII art into a recognizable brand
/// color without requiring a terminfo capability beyond 24-bit truecolor.
/// What: 27-element array of `(r, g, b)` tuples, one per robot row (indexed
/// by row position 0–26); interpolated across three anchor colors.
/// Test: `robot_gradient_has_correct_length`.
pub(crate) const ROBOT_GRADIENT: [(u8, u8, u8); 27] = {
    [
        (183, 65, 14),
        (185, 68, 15),
        (187, 72, 16),
        (189, 76, 17),
        (191, 80, 18),
        (193, 83, 19),
        (195, 87, 20),
        (197, 91, 21),
        (199, 94, 22),
        (201, 97, 24),
        (202, 98, 25),
        (203, 99, 27),
        (204, 99, 28),
        (205, 100, 30),
        (206, 103, 32),
        (208, 106, 34),
        (210, 109, 36),
        (212, 112, 38),
        (214, 115, 40),
        (216, 118, 43),
        (217, 121, 46),
        (219, 124, 48),
        (220, 127, 50),
        (221, 130, 53),
        (222, 133, 55),
        (223, 137, 58),
        (224, 140, 60),
    ]
};

/// Colorize a single robot-art row with the rust gradient.
///
/// Why: applying the gradient row-by-row produces a smooth top-to-bottom
/// color wash without any per-character logic.
/// What: looks up `(r, g, b)` from [`ROBOT_GRADIENT`] for `row_idx` (clamped
/// to 26) and applies `colored::Colorize::truecolor` to the line. When color
/// is disabled (e.g. `NO_COLOR` or non-TTY), `colored` degrades gracefully to
/// plain text.
/// Test: `robot_row_colorize_produces_escape_sequences`,
/// `robot_row_colorize_plain_when_disabled`.
pub(crate) fn colorize_robot_row(row_idx: usize, line: &str) -> String {
    let idx = row_idx.min(ROBOT_GRADIENT.len() - 1);
    let (r, g, b) = ROBOT_GRADIENT[idx];
    line.truecolor(r, g, b).to_string()
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

/// Render the full-screen `tm launch` banner into a single string.
///
/// Why: keeping the banner pure (string in, string out) makes it trivially
/// testable and lets [`print_launch_banner`] stay a thin print wrapper.
/// What: builds the cleared-screen escape sequence, the ASCII robot (rust-
/// gradient colorized), the "TRUSTY" wordmark, and an indented session-info
/// block. When `reconnect_session` is `Some`, a `Status:` row is added and the
/// closing action line reads "Reconnecting..." instead of "Launching claude...".
/// Test: `launch_banner_contains_session_fields`,
/// `launch_banner_marks_reconnect`.
#[allow(dead_code)]
pub(crate) fn render_launch_banner(
    workdir: &str,
    tmux_name: &str,
    prompt_path: Option<&std::path::Path>,
    reconnect_session: Option<&str>,
) -> String {
    let mut out = String::new();
    // Clear the screen and home the cursor so the banner owns the terminal.
    out.push_str("\x1B[2J\x1B[1;1H");

    out.push('\n');
    for (idx, line) in BANNER_ROBOT.iter().enumerate() {
        out.push_str(BANNER_INDENT);
        out.push_str(&colorize_robot_row(idx, line));
        out.push('\n');
    }
    out.push('\n');
    for line in wordmark_lines() {
        out.push_str(BANNER_INDENT);
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');

    let separator = "─".repeat(BANNER_SEPARATOR_WIDTH);
    let field =
        |label: &str, value: &str| -> String { format!("{BANNER_INDENT}{label:<9}:  {value}\n") };

    let memory = detect_memory();
    let search = detect_tool("trusty-search");
    let prompt = match prompt_path {
        Some(p) => p.display().to_string(),
        None => "(default)".to_string(),
    };

    out.push_str(BANNER_INDENT);
    out.push_str(&separator);
    out.push('\n');
    out.push_str(&field("Project", workdir));
    out.push_str(&field("Session", tmux_name));
    if let Some(session) = reconnect_session {
        out.push_str(&field(
            "Status",
            &format!("↩  reconnecting to existing session ({session})"),
        ));
    } else {
        out.push_str(&field("Memory", &format!("{memory}  ✓")));
        out.push_str(&field("Search", &format!("{search}  ✓")));
        out.push_str(&field("Prompt", &prompt));
    }
    out.push_str(BANNER_INDENT);
    out.push_str(&separator);
    out.push('\n');
    out.push('\n');

    let action = if reconnect_session.is_some() {
        "Reconnecting..."
    } else {
        "Launching claude..."
    };
    out.push_str(BANNER_INDENT);
    out.push_str(action);
    out.push('\n');
    out
}

/// Render the robot art + TRUSTY wordmark without the full-screen clear escape.
///
/// Why: `tm banner` preview must not wipe the operator's scrollback. This
/// helper is identical to [`render_robot_splash`] except it omits the
/// `\x1B[2J\x1B[1;1H` sequence at the top.
/// What: colorized robot rows + TRUSTY title block, optionally followed by
/// `"Reconnecting..."` when `reconnecting` is `true`.
/// Test: `banner_preview_does_not_panic`.
pub(crate) fn render_robot_splash_no_clear(reconnecting: bool) -> String {
    let mut out = String::new();
    out.push('\n');
    for (idx, line) in BANNER_ROBOT.iter().enumerate() {
        out.push_str(BANNER_INDENT);
        out.push_str(&colorize_robot_row(idx, line));
        out.push('\n');
    }
    out.push('\n');
    for line in wordmark_lines() {
        out.push_str(BANNER_INDENT);
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');
    if reconnecting {
        out.push_str(BANNER_INDENT);
        out.push_str("Reconnecting...");
        out.push('\n');
    }
    out
}

/// Print the full-screen `tm launch` banner with the rich info panel beneath.
///
/// Why: `tm launch` should give the operator a readable splash screen before
/// the terminal is taken over by `claude`/`tmux`. The robot clears the screen
/// for visual impact; the info panel below provides rich operational context.
/// What: clears the screen, then renders the two-panel layout on wide terminals
/// (≥`two_panel::MIN_WIDTH_TWO_PANEL` cols) or the stacked layout on narrow
/// terminals. Pauses 1 s total (the info-box's own sleep covers it).
/// Test: `launch_banner_does_not_panic`.
pub(crate) fn print_launch_banner(
    workdir: &str,
    tmux_name: &str,
    _prompt_path: Option<&std::path::Path>,
) {
    let w = terminal_width();
    let daemon = super::info_box::DaemonInfo::from_lock_file();
    let data = super::info_box::gather_welcome_data(workdir, tmux_name, false, daemon);

    if let Some(panel) = two_panel::render_two_panel_banner(&data, w, false) {
        println!("\x1B[2J\x1B[1;1H");
        print!("{panel}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::thread::sleep(std::time::Duration::from_secs(1));
    } else {
        // Narrow fallback: stacked layout.
        print!("{}", render_robot_splash(false));
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let daemon2 = super::info_box::DaemonInfo::from_lock_file();
        super::info_box::print_welcome_panel(workdir, tmux_name, false, daemon2);
    }
}

/// Print the full-screen robot banner without the info panel (for tests/reconnect).
///
/// Why: renders the robot + title block as a pure string so test callers can
/// assert content without going through the I/O path.
/// What: `\x1B[2J\x1B[1;1H` clear + colorized robot rows + TRUSTY title.
/// Test: `launch_banner_does_not_panic`, `launch_reconnect_banner_does_not_panic`.
pub(crate) fn render_robot_splash(reconnecting: bool) -> String {
    let mut out = String::new();
    out.push_str("\x1B[2J\x1B[1;1H");
    out.push('\n');
    for (idx, line) in BANNER_ROBOT.iter().enumerate() {
        out.push_str(BANNER_INDENT);
        out.push_str(&colorize_robot_row(idx, line));
        out.push('\n');
    }
    out.push('\n');
    for line in wordmark_lines() {
        out.push_str(BANNER_INDENT);
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');
    if reconnecting {
        out.push_str(BANNER_INDENT);
        out.push_str("Reconnecting...");
        out.push('\n');
    }
    out
}

/// Print the full-screen `tm launch` banner with a "reconnecting" status line.
///
/// Why: when `tm launch` attaches to a pre-existing session the operator should
/// see that no new session was created.
/// What: on wide terminals renders the two-panel layout with the reconnecting
/// state shown in both panels; on narrow terminals falls back to the stacked
/// layout with the rich info-box (reconnect mode).
/// Test: `launch_reconnect_banner_does_not_panic`.
pub(crate) fn print_launch_banner_reconnecting(workdir: &str, tmux_name: &str) {
    let w = terminal_width();
    let daemon = super::info_box::DaemonInfo::from_lock_file();
    let data = super::info_box::gather_welcome_data(workdir, tmux_name, true, daemon);

    if let Some(panel) = two_panel::render_two_panel_banner(&data, w, true) {
        println!("\x1B[2J\x1B[1;1H");
        print!("{panel}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::thread::sleep(std::time::Duration::from_secs(1));
    } else {
        print!("{}", render_robot_splash(false));
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let daemon2 = super::info_box::DaemonInfo::from_lock_file();
        super::info_box::print_welcome_panel(workdir, tmux_name, true, daemon2);
    }
}

/// Print the banner to stdout for preview — no screen-clear, no sleep.
///
/// Why: `tm banner` lets the operator eyeball the colored robot + welcome panel
/// without launching Claude or waiting a full second. Skipping the
/// `\x1B[2J\x1B[1;1H` clear preserves the operator's scrollback, and the
/// absence of the 1-second sleep makes iteration fast.
/// What: on wide terminals renders the two-panel layout (no screen-clear).
/// On narrow terminals falls back to robot art + stacked info box.
/// Test: `banner_preview_does_not_panic` in `tests.rs`.
pub(crate) fn print_banner_preview(reconnecting: bool) {
    let w = terminal_width();
    let workdir = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let session_name = fallback_session_name(std::path::Path::new(&workdir));
    let daemon = super::info_box::DaemonInfo::from_lock_file();
    let data = super::info_box::gather_welcome_data(&workdir, &session_name, reconnecting, daemon);

    if let Some(panel) = two_panel::render_two_panel_banner(&data, w, reconnecting) {
        // No screen-clear for preview — preserve scrollback.
        print!("\n{panel}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    } else {
        // Narrow fallback: stacked layout (no screen-clear).
        print!("{}", render_robot_splash_no_clear(reconnecting));
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let daemon2 = super::info_box::DaemonInfo::from_lock_file();
        super::info_box::print_welcome_panel_no_sleep(
            &workdir,
            &session_name,
            reconnecting,
            daemon2,
        );
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
