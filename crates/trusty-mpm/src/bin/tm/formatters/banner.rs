//! Launch-banner rendering for `tm launch` and `tm connect`.
//!
//! Why: the full-screen ASCII-art banner is ~130 lines of data and ~100 lines
//! of rendering logic; keeping it separate from the launch handler keeps that
//! file under the 500-line cap and makes it trivially testable in isolation.
//! What: `render_launch_banner`, `print_launch_banner`,
//! `print_launch_banner_reconnecting`, `terminal_width`, `detect_memory`,
//! `detect_tool`, `dirs_config_dir`, `binary_on_path`, `fallback_session_name`,
//! `normalize_workdir`, `tmux_has_session`.
//! Test: `launch_banner_*`, `terminal_width_is_positive`,
//! `normalize_workdir_strips_trailing_slash` in `tests.rs`.

/// Left indent applied to every line of the full-screen launch banner.
pub(crate) const BANNER_INDENT: &str = "   ";

/// Width of the session-info separator line drawn in the launch banner.
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

/// The block-character "TRUSTY" wordmark drawn below the robot.
///
/// Why: a large title makes the banner read as a deliberate splash screen
/// rather than a stray log line.
/// What: six rows of unicode block-drawing glyphs plus a spaced-out subtitle.
pub(crate) const BANNER_TITLE: &[&str] = &[
    "████████╗██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗",
    "╚══██╔══╝██╔══██╗██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝",
    "   ██║   ██████╔╝██║   ██║███████╗   ██║    ╚████╔╝ ",
    "   ██║   ██╔══██╗██║   ██║╚════██║   ██║     ╚██╔╝  ",
    "   ██║   ██║  ██║╚██████╔╝███████║   ██║      ██║   ",
    "   ╚═╝   ╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ",
    "             M U L T I - A G E N T   P M",
];

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
/// What: builds the cleared-screen escape sequence, the ASCII robot, the
/// "TRUSTY" wordmark, and an indented session-info block. When
/// `reconnect_session` is `Some`, a `Status:` row is added and the closing
/// action line reads "Reconnecting..." instead of "Launching claude...".
/// Test: `launch_banner_contains_session_fields`,
/// `launch_banner_marks_reconnect`.
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
    for line in BANNER_ROBOT {
        out.push_str(BANNER_INDENT);
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    for line in BANNER_TITLE {
        out.push_str(BANNER_INDENT);
        out.push_str(line);
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

/// Print the full-screen `tm launch` banner, then pause briefly.
///
/// Why: `tm launch` should give the operator a readable splash screen before
/// the terminal is taken over by `claude`/`tmux`.
/// What: clears the screen, prints the ASCII robot, the "TRUSTY" wordmark, and
/// the indented session-info block, queries the terminal width once (kept for
/// future centering), then sleeps one second so the banner is legible.
/// Test: `launch_banner_does_not_panic`.
pub(crate) fn print_launch_banner(
    workdir: &str,
    tmux_name: &str,
    prompt_path: Option<&std::path::Path>,
) {
    let _ = terminal_width();
    print!(
        "{}",
        render_launch_banner(workdir, tmux_name, prompt_path, None)
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::thread::sleep(std::time::Duration::from_secs(1));
}

/// Print the full-screen `tm launch` banner with a "reconnecting" status line.
///
/// Why: when `tm launch` attaches to a pre-existing session the operator should
/// see that no new session was created.
/// What: prints the same full-screen banner as [`print_launch_banner`] but with
/// a `Status:` row noting the reconnect, then pauses one second so the banner
/// is legible before `tmux` takes over.
/// Test: `launch_reconnect_banner_does_not_panic`.
pub(crate) fn print_launch_banner_reconnecting(workdir: &str, tmux_name: &str) {
    let _ = terminal_width();
    print!(
        "{}",
        render_launch_banner(workdir, tmux_name, None, Some(tmux_name))
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::thread::sleep(std::time::Duration::from_secs(1));
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
