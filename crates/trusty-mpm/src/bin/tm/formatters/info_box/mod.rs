//! Pre-launch welcome panel for `tm launch`, `tm connect`, and bare `tm`.
//!
//! Why: operators need project context (version, project, recent commits,
//! service status, and command reminders) before tmux takes over the terminal;
//! a compact `╭─╮` box conveys that without the old full-screen screen-wipe.
//! What: `WelcomeData` holds all panel data; `gather_welcome_data` does bounded
//! probes (≤150 ms each); `render_welcome_panel` builds the box string (pure);
//! `print_welcome_panel` = gather + render + flush + sleep. `DaemonInfo` reads
//! the lock file and optionally probes the HTTP API.
//! Test: `welcome_panel_renders_*` in `render.rs`; `probe_*` in `probes.rs`;
//! backward-compat `info_box_renders_*` in the inline tests below.

mod probes;
pub(crate) mod render;

use std::path::Path;
use std::time::Duration;

// ── Logo ──────────────────────────────────────────────────────────────────────

/// Three-line block-element logo, each exactly 13 display columns wide.
///
/// Why: fills the left column of the panel, giving `tm` a visual identity.
/// What: block-drawing characters space-padded to a uniform 13-column width.
/// Test: `welcome_panel_renders_online` checks the box renders without panic.
pub(crate) const LOGO: [&str; 3] = [
    "▐▛███▜▌      ", // 7 display cols + 6 spaces = 13
    "▝▜█████▛▘    ", // 9 display cols + 4 spaces = 13
    "▘▘  ▝▝       ", // 6 display cols + 7 spaces = 13
];

/// Display-column width of each logo line.
pub(crate) const LOGO_COLS: usize = 13;

// ── DaemonInfo ────────────────────────────────────────────────────────────────

/// Live state of the trusty-mpm daemon for banner display.
///
/// Why: the welcome panel shows daemon reachability and session count; this
/// struct decouples the probe logic from the rendering logic so both are
/// testable independently.
/// What: holds the bound address, optional session count, and an online flag.
/// Test: `welcome_panel_renders_online` / `welcome_panel_renders_offline`.
#[derive(Debug, Default)]
pub(crate) struct DaemonInfo {
    /// The address the daemon is bound to (e.g. `127.0.0.1:7880`).
    pub(crate) addr: String,
    /// Number of active sessions when the HTTP probe succeeded.
    pub(crate) session_count: Option<usize>,
    /// True when the lock file reports a live PID.
    pub(crate) online: bool,
}

impl DaemonInfo {
    /// Read daemon status from `~/.trusty-mpm/daemon.lock`.
    ///
    /// Why: the lock file is cheap to read and available without an HTTP
    /// round-trip; it gives us the addr and PID before we probe the live API.
    /// What: parses `addr = "..."` and `pid = N`, checks PID liveness on Unix,
    /// and sets `online` accordingly. Returns the zero-value when absent/stale.
    /// Test: `welcome_panel_renders_online` / `welcome_panel_renders_offline`.
    pub(crate) fn from_lock_file() -> Self {
        match read_lock_addr() {
            Some(addr) => DaemonInfo {
                addr,
                online: true,
                session_count: None,
            },
            None => DaemonInfo::default(),
        }
    }

    /// Attach a known session count (e.g. from the guided flow's session list).
    ///
    /// Why: the guided flow already fetched the session list so there is no need
    /// to fire a second HTTP probe just to fill the count.
    /// What: sets `session_count` to `Some(count)` and returns `self`.
    /// Test: `welcome_panel_renders_online` (called with `.with_count(2)`).
    pub(crate) fn with_count(mut self, count: usize) -> Self {
        self.session_count = Some(count);
        self
    }

    /// Probe `/sessions` to attach a session count (best-effort, ≤150 ms).
    ///
    /// Why: `tm launch` and `tm connect` do not have a pre-fetched session list,
    /// so a cheap HTTP probe fills in the count for the panel.
    /// What: spawns a background thread (blocking reqwest, 150 ms timeout);
    /// waits up to 200 ms for the result; degrades gracefully on timeout/error.
    /// Test: `welcome_panel_renders_online` verifies this does not panic.
    pub(crate) fn probe_session_count(mut self, base_url: &str) -> Self {
        if self.online {
            self.session_count = probes::try_get_session_count(base_url);
        }
        self
    }
}

// ── WelcomeData ───────────────────────────────────────────────────────────────

/// All data required to render the pre-launch welcome panel (pure input).
///
/// Why: separating data gathering from rendering keeps `render_welcome_panel`
/// pure and unit-testable with no I/O.
/// What: holds project identity, workspace, user, daemon state, recent commits,
/// service status strings, and the reconnect flag + session name.
/// Test: constructed in every `render.rs` test.
pub(crate) struct WelcomeData {
    /// Short project name or `owner/repo` identity.
    pub(crate) project: String,
    /// Working directory shown in the panel (may be abbreviated with `~`).
    pub(crate) workspace: String,
    /// Login username, or empty string when `$USER` is unset.
    pub(crate) user: String,
    /// True when the panel is shown for a reconnect rather than a new launch.
    pub(crate) reconnecting: bool,
    /// Tmux session name shown in the reconnect row (only when `reconnecting`).
    pub(crate) session_name: String,
    /// Daemon state (address, session count, online flag).
    pub(crate) daemon: DaemonInfo,
    /// Recent git commits from the project repo (empty when git is unavailable).
    pub(crate) recent_commits: Vec<CommitLine>,
    /// `detect_memory()` result: service name or `"(not detected)"`.
    pub(crate) memory_status: String,
    /// `detect_tool("trusty-search")` result.
    pub(crate) search_status: String,
    /// `detect_tool("trusty-review")` result.
    pub(crate) review_status: String,
}

/// One line from `git log`, parsed into display fields.
///
/// Why: a typed struct makes tests readable and avoids triple-string tuples.
/// What: short sha (7 chars), compact age (e.g. `"2h"`), truncated subject.
/// Test: constructed in `welcome_panel_shows_commits` in `render.rs`.
#[derive(Debug)]
pub(crate) struct CommitLine {
    pub(crate) sha: String,
    pub(crate) age: String,
    pub(crate) subject: String,
}

// ── Data gathering ────────────────────────────────────────────────────────────

/// Gather all panel data via bounded probes, then return `WelcomeData`.
///
/// Why: separating gathering from rendering lets us unit-test the renderer with
/// hand-crafted `WelcomeData` values.
/// What: reads `$USER`, derives the `owner/repo` identity from the git remote
/// (falls back to the folder name when outside a git repo), resolves the
/// managed workspace path (`~/trusty-mpm-projects/<owner>/<repo>`), probes
/// recent commits (≤150 ms), and detects services using the banner helpers.
/// All probes are time-bounded — the function never blocks the launch path.
/// Test: indirectly by `print_welcome_panel` smoke tests; the owner/repo and
/// managed-path derivations are exercised by the underlying helpers in
/// `trusty_common::github_path` and `trusty_mpm::core::trusty_tools_config`.
pub(crate) fn gather_welcome_data(
    workdir: &str,
    session_name: &str,
    reconnecting: bool,
    daemon: DaemonInfo,
) -> WelcomeData {
    let workdir_path = Path::new(workdir);

    // Derive the GitHub `owner/repo` identity from the git remote.  Falls back
    // to the folder name when the directory is not a git repo or has no origin.
    let github_path = trusty_common::github_path::derive_github_path(workdir_path);
    let project = github_path
        .as_ref()
        .map(|gp| gp.rel_path())
        .unwrap_or_else(|| {
            workdir_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| workdir.to_string())
        });

    // Resolve the managed workspace path (`~/trusty-mpm-projects/<owner>/<repo>`).
    // This is where trusty-mpm manages a clone of the project independently of
    // the user's live checkout; the banner should show this path rather than the
    // live cwd.  Falls back to `workdir` when the GitHub identity cannot be
    // derived (e.g. no git remote).
    let workspace = match &github_path {
        Some(gp) => {
            let cfg = trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load();
            let managed = trusty_mpm::core::trusty_tools_config::workspace_subpath(&cfg, gp);
            managed.to_string_lossy().into_owned()
        }
        None => workdir.to_string(),
    };

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();

    let recent_commits = probes::probe_recent_commits(workdir);
    let memory_status = super::banner::detect_memory();
    let search_status = super::banner::detect_tool("trusty-search");
    let review_status = super::banner::detect_tool("trusty-review");

    WelcomeData {
        project,
        workspace,
        user,
        reconnecting,
        session_name: session_name.to_string(),
        daemon,
        recent_commits,
        memory_status,
        search_status,
        review_status,
    }
}

// ── Print entry points ────────────────────────────────────────────────────────

/// Print the welcome panel to stdout, flush, and pause 1 second.
///
/// Why: the panel must be visible before tmux takes over the terminal; the
/// 1-second pause gives the operator time to read it (#1743).
/// What: calls `gather_welcome_data`, then `render_welcome_panel`, prints
/// the result to stdout, flushes, and sleeps 1 s.
/// Test: `welcome_panel_renders_online` (no panic; sleep not unit-tested).
pub(crate) fn print_welcome_panel(
    workdir: &str,
    session_name: &str,
    reconnecting: bool,
    daemon: DaemonInfo,
) {
    let data = gather_welcome_data(workdir, session_name, reconnecting, daemon);
    print!("{}", render::render_welcome_panel(&data));
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::thread::sleep(Duration::from_secs(1));
}

/// Backward-compatible alias — calls `print_welcome_panel`.
///
/// Why: existing call sites in `launch.rs` and `guided.rs` use `print_info_box`;
/// this alias avoids touching those call sites in the same commit.
/// What: delegates directly to `print_welcome_panel` with identical parameters.
/// Test: covered by the same paths as `print_welcome_panel`.
pub(crate) fn print_info_box(
    workdir: &str,
    session_name: &str,
    reconnecting: bool,
    daemon: &DaemonInfo,
) {
    // Rebuild DaemonInfo from its fields (no Clone derive on purpose — keeps the
    // struct lean; the call sites always have a fresh DaemonInfo).
    let d = DaemonInfo {
        addr: daemon.addr.clone(),
        online: daemon.online,
        session_count: daemon.session_count,
    };
    print_welcome_panel(workdir, session_name, reconnecting, d);
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Replace the home-directory prefix in `path` with `~`.
///
/// Why: workspace paths are long; abbreviating the home prefix makes the panel
/// narrower and more readable.
/// What: detects the `$HOME` prefix and replaces it with `~`.
/// Test: covered indirectly by welcome-panel render tests.
pub(crate) fn abbreviate_home(path: &str) -> String {
    let home = match dirs::home_dir() {
        Some(h) => h.to_string_lossy().into_owned(),
        None => return path.to_string(),
    };
    if path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

/// Read `addr` from `~/.trusty-mpm/daemon.lock`, verifying PID liveness.
///
/// Why: the lock file is cheaper than an HTTP probe and tells us the daemon
/// address before we decide whether to fire the probe.
/// What: parses `addr = "..."` and `pid = N`; on Unix validates the PID with
/// `kill(pid, 0)`; returns `None` when the lock file is absent or PID is stale.
/// Test: covered indirectly by the welcome-panel render tests.
fn read_lock_addr() -> Option<String> {
    let path = trusty_mpm::core::lock_file_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let mut addr: Option<String> = None;
    let mut pid: Option<u32> = None;
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("addr = ") {
            addr = Some(v.trim_matches('"').to_string());
        }
        if let Some(v) = line.strip_prefix("pid = ") {
            pid = v.trim().parse().ok();
        }
    }
    #[cfg(unix)]
    if let Some(p) = pid
        && unsafe { libc::kill(p as libc::pid_t, 0) } != 0
    {
        return None;
    }
    let _ = pid;
    addr
}

// ── Two-panel compositor bridge ───────────────────────────────────────────────

/// Return the welcome-panel content rows without drawing a box frame.
///
/// Why: the two-panel banner compositor (`banner::two_panel`) needs the raw
/// row strings to fill the right panel at the compositor-determined width,
/// without the fixed-width `╭─╮` box that `render_welcome_panel` draws.
/// The first row (`"trusty-mpm vX.Y.Z"`) is omitted here because the
/// two-panel banner already embeds the version in its title bar border — the
/// standalone `render_welcome_panel` box (no title bar) keeps it via
/// `build_rows` directly.
/// What: calls `render::build_rows(data)`, drops the first element (the
/// version line), and returns the remainder. Content is otherwise identical
/// to the standalone panel, so both layouts share the same row-building logic.
/// Test: `two_panel_compose_alignment` in `banner::two_panel::tests`;
/// `right_panel_starts_with_welcome` in `banner::two_panel::tests`.
pub(crate) fn render_info_box_rows(data: &WelcomeData) -> Vec<String> {
    let mut rows = render::build_rows(data);
    // Drop the "trusty-mpm vX.Y.Z" header — the two-panel title bar already
    // shows it. The standalone render_welcome_panel (no title bar) retains it
    // by calling build_rows directly without going through this function.
    if !rows.is_empty() {
        rows.remove(0);
    }
    rows
}

// ── Backward-compat render wrapper ────────────────────────────────────────────

/// Render the welcome panel — thin wrapper over `render::render_welcome_panel`.
///
/// Why: kept for the inline tests below that previously called `render_info_box`.
/// What: builds `WelcomeData` from the legacy four-parameter signature and
/// delegates to the new pure renderer; services are probed at render time.
/// Test: `info_box_renders_online`, `info_box_renders_offline`,
/// `info_box_renders_reconnecting`, `info_box_renders_with_count`.
#[cfg(test)]
pub(crate) fn render_info_box(
    workdir: &str,
    tmux_name: &str,
    reconnecting: bool,
    daemon: &DaemonInfo,
) -> String {
    let data = WelcomeData {
        project: Path::new(workdir)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| workdir.to_string()),
        workspace: workdir.to_string(),
        user: String::new(),
        reconnecting,
        session_name: tmux_name.to_string(),
        daemon: DaemonInfo {
            addr: daemon.addr.clone(),
            online: daemon.online,
            session_count: daemon.session_count,
        },
        recent_commits: vec![],
        memory_status: super::banner::detect_memory(),
        search_status: super::banner::detect_tool("trusty-search"),
        review_status: super::banner::detect_tool("trusty-review"),
    };
    render::render_welcome_panel(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn online(port: u16) -> DaemonInfo {
        DaemonInfo {
            addr: format!("127.0.0.1:{port}"),
            online: true,
            session_count: None,
        }
    }

    fn offline() -> DaemonInfo {
        DaemonInfo::default()
    }

    #[test]
    fn info_box_renders_online() {
        let out = render_info_box(
            "/home/user/my-project",
            "tmpm-my-project",
            false,
            &online(7880),
        );
        assert!(out.contains('\u{25cf}'), "expected ● online marker");
        assert!(out.contains(":7880"), "expected port in daemon row");
        assert!(out.contains("my-project"), "expected project name");
        assert!(!out.contains("reconnecting"), "must not show reconnecting");
    }

    #[test]
    fn info_box_renders_offline() {
        let out = render_info_box(
            "/home/user/my-project",
            "tmpm-my-project",
            false,
            &offline(),
        );
        assert!(out.contains('\u{25cb}'), "expected ○ marker");
        assert!(out.contains("offline"), "expected offline label");
        assert!(!out.contains("reconnecting"), "must not show reconnecting");
    }

    #[test]
    fn info_box_renders_reconnecting() {
        let out = render_info_box("/home/user/my-project", "tmpm-my-project", true, &offline());
        assert!(out.contains("reconnecting"), "expected reconnecting hint");
        assert!(
            out.contains("tmpm-my-project"),
            "expected session name in reconnect row"
        );
    }

    #[test]
    fn info_box_renders_with_count() {
        let d = online(7880).with_count(3);
        let out = render_info_box("/home/user/proj", "sess", false, &d);
        assert!(out.contains("(3)"), "expected session count");
    }

    #[test]
    fn info_box_no_checkmark_on_not_detected_lines() {
        let out = render_info_box("/tmp/proj", "sess", false, &offline());
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
    fn abbreviate_home_replaces_prefix() {
        if let Some(home) = dirs::home_dir() {
            let path = format!("{}/projects/repo", home.display());
            let result = abbreviate_home(&path);
            assert!(result.starts_with('~'), "expected ~ prefix: {result:?}");
        }
    }
}
