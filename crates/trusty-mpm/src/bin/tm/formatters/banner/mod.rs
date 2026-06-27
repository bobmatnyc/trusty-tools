//! Launch-banner rendering for `tm launch` and `tm connect`.
//!
//! Why: the full-screen ASCII-art banner is ~130 lines of data and ~80 lines
//! of rendering logic; keeping it separate from the launch handler keeps that
//! file under the 500-line cap and makes it trivially testable in isolation.
//! What: `print_launch_banner`, `print_launch_banner_reconnecting`,
//! `print_banner_preview`, `terminal_width`, `detect_memory`, `detect_tool`,
//! `dirs_config_dir`, `binary_on_path`, `fallback_session_name`,
//! `normalize_workdir`, `tmux_has_session`. The single-box compositor lives in
//! the `two_panel` submodule.
//! Test: `launch_banner_*`, `terminal_width_is_positive`,
//! `normalize_workdir_strips_trailing_slash` in `tests.rs`;
//! `two_panel_*` in `two_panel::tests`.

pub(crate) mod two_panel;

use colored::Colorize as _;

/// The ASCII-art robot mascot drawn at the top of the launch banner.
///
/// Why: a recognizable centerpiece gives `tm launch` the same "the tool has
/// taken over the terminal" feel as claude-mpm's startup screen.
/// What: a multi-line string-art robot; each line is 45 display columns wide.
/// Test: `robot_shrink_has_fewer_rows` in `two_panel::tests`.
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

/// Rust-color gradient palette applied row-by-row across the robot art.
///
/// Why: a smooth gradient from dark rust (top) through burnt orange (middle)
/// to amber (lower) turns the monochrome ASCII art into a recognizable brand
/// color without requiring a terminfo capability beyond 24-bit truecolor.
/// What: 27-element array of `(r, g, b)` tuples, one per robot row (0–26);
/// each shrunk row looks up its original index via `colorize_robot_row`.
/// Test: `robot_gradient_matches_original_row_count` in `two_panel::tests`.
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
/// Test: `robot_row_colorize_preserves_text`.
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
    let data = super::info_box::gather_welcome_data(workdir, tmux_name, true, daemon);

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

/// Print the banner to stdout for preview — no screen-clear, no sleep.
///
/// Why: `tm banner` lets the operator eyeball the colored robot + welcome panel
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
    let data = super::info_box::gather_welcome_data(&workdir, &session_name, reconnecting, daemon);

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
