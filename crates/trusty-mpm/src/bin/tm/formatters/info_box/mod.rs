//! Pre-launch welcome panel for `tm launch`, `tm connect`, and bare `tm`.
//!
//! Why: operators need project context (version, project, recent commits,
//! service status, and command reminders) before tmux takes over the terminal;
//! a compact `╭─╮` box conveys that without the old full-screen screen-wipe.
//! What: `WelcomeData` holds all panel data; `gather_welcome_data` does bounded
//! probes (≤150 ms each); `render_welcome_panel` builds the box string (pure).
//! `DaemonInfo` reads the lock file and optionally probes the HTTP API.
//! The no-subcommand `tm` daily banner uses `banner::print_daily_banner` which
//! routes through `render_two_panel_banner` instead of the compact LOGO path
//! (#1808). `print_launch_banner` / `print_launch_banner_reconnecting` in
//! `banner/mod.rs` handle `tm launch` and `tm connect` banners.
//! Test: `welcome_panel_renders_*` in `render.rs`; `probe_*` in `probes.rs`;
//! backward-compat `info_box_renders_*` in the inline tests below.

mod probes;
pub(crate) mod render;

use std::path::Path;

// ── Logo ──────────────────────────────────────────────────────────────────────

/// Three-line block-element logo, each exactly 13 display columns wide.
///
/// Why: fills the left column of the panel, giving `tm` a visual identity.
/// What: block-drawing characters space-padded to a uniform 13-column width.
/// Test: `welcome_panel_renders_online` checks the box renders without panic.
/// Only consumed by the test-only `render_welcome_panel` box renderer.
#[cfg(test)]
pub(crate) const LOGO: [&str; 3] = [
    "▐▛███▜▌      ", // 7 display cols + 6 spaces = 13
    "▝▜█████▛▘    ", // 9 display cols + 4 spaces = 13
    "▘▘  ▝▝       ", // 6 display cols + 7 spaces = 13
];

/// Display-column width of each logo line.
/// Only consumed by the test-only `render_welcome_panel` box renderer.
#[cfg(test)]
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

    /// Probe daemon status via lock file addr + TCP connect (150 ms timeout).
    ///
    /// Why: `from_lock_file()` only checks PID liveness, which can disagree with
    /// `tm health` when the binary was reinstalled or the PID drifted (#1839).
    /// A TCP connect to the lock-file address (or the env/default URL when absent)
    /// gives the same "is the port alive?" answer that the HTTP health probe uses,
    /// so the banner and `tm health` agree on the daemon status.
    /// What: reads `read_lock_addr()` for the addr; TCP-probes the result. When
    /// the lock file is absent or the PID is stale, falls back to probing the
    /// `TRUSTY_MPM_URL` env var or `DEFAULT_DAEMON_URL`. Sets `online` from the
    /// TCP connect result (not the PID check).
    /// Test: `probe_with_nonlistening_addr_returns_offline`.
    pub(crate) fn from_lock_file_with_probe() -> Self {
        let addr = read_lock_addr().or_else(probe_default_addr);
        let Some(addr) = addr else {
            return DaemonInfo::default();
        };
        let online = tcp_probe(&addr);
        DaemonInfo {
            addr,
            online,
            session_count: None,
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
    managed_path: Option<&Path>,
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

    // Resolve the workspace path shown in the panel.
    //
    // When `managed_path` is provided (i.e. the caller already provisioned the
    // session worktree), display it verbatim — this is the real session directory
    // (`<base>/.worktrees/<session-id>/`) and must NOT be re-derived via
    // `workspace_subpath` which would strip the session subdir (#1803, #1794).
    //
    // When `managed_path` is None (pre-launch picker, reconnect banner, or any
    // context where no session has been provisioned yet), fall back to deriving
    // the repo base `~/trusty-mpm-projects/<owner>/<repo>` via `workspace_subpath`.
    let workspace = if let Some(mp) = managed_path {
        mp.to_string_lossy().into_owned()
    } else {
        match &github_path {
            Some(gp) => {
                let cfg = trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load();
                let managed = trusty_mpm::core::trusty_tools_config::workspace_subpath(&cfg, gp);
                managed.to_string_lossy().into_owned()
            }
            None => workdir.to_string(),
        }
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

// ── TCP probe helpers ─────────────────────────────────────────────────────────

/// Attempt a TCP connect to `addr` (host:port) with a 150 ms timeout.
///
/// Why: the banner must show the daemon as online only when the port is actually
/// reachable — PID liveness alone does not guarantee the HTTP server is up (#1839).
/// What: resolves `addr` (a `host:port` string) via `ToSocketAddrs` — supporting
/// both numeric IPs and hostnames such as `localhost` — then calls
/// `TcpStream::connect_timeout` on the first resolved address.
/// Returns `true` on success, `false` on any error (bad addr, connection refused, …).
/// Test: `probe_with_nonlistening_addr_returns_offline`.
pub(crate) fn tcp_probe(addr: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs as _};
    use std::time::Duration;
    addr.to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .map(|sa| TcpStream::connect_timeout(&sa, Duration::from_millis(150)).is_ok())
        .unwrap_or(false)
}

/// Build the fallback addr string (host:port) from env or the default URL.
///
/// Why: when the lock file is absent, we still try to probe the well-known
/// default address so the banner shows the correct online state even when the
/// daemon was started without a lock file or with a stale PID.
/// What: reads `TRUSTY_MPM_URL` env var first (falls back to
/// `DEFAULT_DAEMON_URL`), then calls `url_to_probe_addr` to extract host:port.
/// Test: `probe_default_addr_handles_url_without_port` (via the inner helper).
fn probe_default_addr() -> Option<String> {
    let url = std::env::var("TRUSTY_MPM_URL")
        .unwrap_or_else(|_| trusty_mpm::core::DEFAULT_DAEMON_URL.to_string());
    url_to_probe_addr(&url)
}

/// Extract a `host:port` string from an HTTP URL, defaulting the port when absent.
///
/// Why: `TRUSTY_MPM_URL` may carry a portless URL such as `http://localhost/`;
/// bare-hostname `SocketAddr::parse` fails, making the banner show offline even
/// when the daemon is up (#1839 review). Extracting this as a pure function also
/// makes it unit-testable without env-var manipulation.
/// What: strips `http(s)://`, takes the authority (everything before the first `/`),
/// and appends the daemon's default port (from `DEFAULT_DAEMON_ADDR`) when no `:port`
/// is present in the authority. IPv6 bracket notation (`[::1]:port`) always has `:`
/// and is therefore unaffected.
/// Test: `url_to_probe_addr_handles_url_without_port`,
/// `url_to_probe_addr_preserves_explicit_port`.
pub(crate) fn url_to_probe_addr(url: &str) -> Option<String> {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    // Authority ends at the first '/' (or at EOL when there is no path).
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    if authority.is_empty() {
        return None;
    }
    // Append the default port when none is present.
    let with_port = if authority.contains(':') {
        authority.to_string()
    } else {
        let default_port = trusty_mpm::core::DEFAULT_DAEMON_ADDR
            .rsplit(':')
            .next()
            .unwrap_or("7880");
        format!("{authority}:{default_port}")
    };
    Some(with_port)
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
    fn probe_with_nonlistening_addr_returns_offline() {
        // Port 1 requires root to bind; on non-root systems connect always
        // fails immediately — safe to use as a "definitely offline" probe.
        assert!(
            !tcp_probe("127.0.0.1:1"),
            "TCP probe to port 1 must return offline (connection refused)"
        );
    }

    #[test]
    fn probe_with_invalid_addr_returns_offline() {
        assert!(
            !tcp_probe("not-a-valid-addr"),
            "TCP probe to invalid addr must return offline"
        );
    }

    #[test]
    fn abbreviate_home_replaces_prefix() {
        if let Some(home) = dirs::home_dir() {
            let path = format!("{}/projects/repo", home.display());
            let result = abbreviate_home(&path);
            assert!(result.starts_with('~'), "expected ~ prefix: {result:?}");
        }
    }

    /// Portless URL yields a host:port with the default daemon port appended.
    ///
    /// Why: `TRUSTY_MPM_URL=http://localhost/` previously caused `SocketAddr::parse`
    /// to fail in `tcp_probe`, making the banner show offline even when the daemon
    /// was up (#1839 review). `tcp_probe` now uses `ToSocketAddrs` (supports hostnames),
    /// so we verify that `url_to_probe_addr` adds a port — not that it produces a
    /// numeric IP (hostnames like `localhost` are valid probe targets).
    #[test]
    fn url_to_probe_addr_handles_url_without_port() {
        let addr = url_to_probe_addr("http://localhost/")
            .expect("should return Some for http://localhost/");
        assert!(
            addr.contains(':'),
            "addr must include a port separator: {addr:?}"
        );
        // The port portion must be a valid u16 — tcp_probe passes it to to_socket_addrs.
        let port_str = addr.rsplit(':').next().expect("addr has a colon");
        port_str
            .parse::<u16>()
            .expect("port portion of url_to_probe_addr output must be a valid u16");
    }

    /// URL with an explicit port must pass through unchanged (no double-port).
    #[test]
    fn url_to_probe_addr_preserves_explicit_port() {
        let addr = url_to_probe_addr("http://127.0.0.1:9999/")
            .expect("should return Some for http://127.0.0.1:9999/");
        assert_eq!(addr, "127.0.0.1:9999");
    }

    /// Trailing path segments are stripped so only the authority remains.
    #[test]
    fn url_to_probe_addr_strips_path() {
        let addr =
            url_to_probe_addr("http://127.0.0.1:7880/api/v1/health").expect("should return Some");
        assert_eq!(addr, "127.0.0.1:7880");
    }

    /// Empty or scheme-only input returns None.
    #[test]
    fn url_to_probe_addr_empty_returns_none() {
        assert!(url_to_probe_addr("").is_none());
        assert!(url_to_probe_addr("http://").is_none());
        assert!(url_to_probe_addr("http:///").is_none());
    }
}
