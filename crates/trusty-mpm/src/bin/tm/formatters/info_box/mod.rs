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

    /// Probe daemon status by dialling its unix socket (#6288).
    ///
    /// Why: `from_lock_file()` only checks PID liveness, which can disagree with
    /// `tm health` when the binary was reinstalled or the PID drifted (#1839).
    /// A connect to the daemon socket gives the same "is it serving?" answer
    /// `tm health` gets, and — since #6288 — over the same transport, so the
    /// banner never TCP-probes `127.0.0.1:7880`.
    /// What: resolves the socket as every client does
    /// ([`trusty_mpm::core::try_resolve_daemon_url`]; a #9556 sandbox refusal
    /// is offline), then [`socket_probe`]s it. `addr` is the socket locator.
    /// Test: `socket_probe_reports_a_listening_socket_online`,
    /// `socket_probe_reports_an_absent_or_stale_socket_offline`.
    pub(crate) fn from_lock_file_with_probe() -> Self {
        let Ok(addr) = trusty_mpm::core::try_resolve_daemon_url(None) else {
            return DaemonInfo::default();
        };
        let online = trusty_mpm::core::socket_from_locator(&addr).is_some_and(|p| socket_probe(&p));
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
}

// ── ConsoleInfo ───────────────────────────────────────────────────────────────

/// Live state of the trusty-console gateway for banner display.
///
/// Why (#6761): the console is the one service in the block an operator OPENS,
/// and it cannot be opened without its port. The daemon's port is deliberately
/// hidden (#6869) because nothing the operator does needs it; the console's is
/// the opposite case, so the two rows differ on purpose rather than by
/// oversight. The port is read from the console's own discovery record rather
/// than hardcoded, so a console moved off 7788 is still reachable from what the
/// banner prints.
/// What: the `host:port` [`trusty_mpm::core::discovery::console_addr`] resolves,
/// plus the TCP-probe result for it. `Default` is the offline zero value, which
/// is also what the renderer shows when the probe fails.
/// Test: `welcome_panel_renders_the_console_row_with_its_port`,
/// `welcome_panel_console_row_offline_names_no_port`.
#[derive(Debug, Default)]
pub(crate) struct ConsoleInfo {
    /// The address the console is selected to serve on (e.g. `127.0.0.1:7788`).
    pub(crate) addr: String,
    /// True when a TCP connect to `addr` succeeded inside the probe timeout.
    pub(crate) online: bool,
}

impl ConsoleInfo {
    /// Resolve the console's address and probe it (≤150 ms).
    ///
    /// Why: the banner may never block the launch path, and the console is a
    /// plain HTTP listener, so the same bounded [`tcp_probe`] the daemon row
    /// uses answers "is it up?" without an HTTP round trip.
    /// What: [`trusty_mpm::core::discovery::console_addr`] for the address —
    /// the discovery record when one exists, its documented default otherwise —
    /// then one TCP connect. Fail-open in the display sense: any probe failure
    /// renders as offline, never as an error or a hang.
    /// Test: `welcome_panel_console_row_offline_names_no_port` covers the
    /// rendered halves; the address rule is covered by `console_addr_from_*` in
    /// `trusty_mpm::core::discovery`.
    pub(crate) fn from_discovery_with_probe() -> Self {
        // #9556: no target (isolated, nothing recorded) means no probe at all.
        Self::probe(trusty_mpm::core::console_addr_to_probe())
    }

    /// Probe `addr`, or report offline without probing when there is none.
    /// Test: `console_info_without_a_target_is_offline_and_unprobed`.
    fn probe(addr: Option<String>) -> Self {
        let Some(addr) = addr else {
            return ConsoleInfo::default();
        };
        let online = tcp_probe(&addr);
        ConsoleInfo { addr, online }
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
    /// Console gateway state (address, online flag) — #6761.
    pub(crate) console: ConsoleInfo,
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
    // #6761: the console row is probed like the daemon row, and bounded the
    // same way, so the launch path pays at most one more 150 ms connect.
    let console = ConsoleInfo::from_discovery_with_probe();

    WelcomeData {
        project,
        workspace,
        user,
        reconnecting,
        session_name: session_name.to_string(),
        daemon,
        console,
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

/// Dial the daemon socket once: `true` when something accepts the connection.
///
/// Why: the banner's daemon dot (#6288). The hardened pre-connect check runs
/// first, so a foreign-owned or wrong-mode socket reads offline rather than
/// being dialled.
/// What: [`trusty_common::uds::verify_socket_for_connect`], then a blocking
/// `UnixStream::connect`, which on a local socket returns at once: refused
/// for a stale file, `ENOENT` for an absent one.
/// Test: `socket_probe_reports_a_listening_socket_online`,
/// `socket_probe_reports_an_absent_or_stale_socket_offline`.
pub(crate) fn socket_probe(path: &std::path::Path) -> bool {
    trusty_common::uds::verify_socket_for_connect(path).is_ok()
        && std::os::unix::net::UnixStream::connect(path).is_ok()
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
/// What: delegates to `core::client_lock_url`, which reads the record through
/// `core::daemon_identity::read_lock` (product magic, live PID) and, under
/// isolation, drops a lock that belongs to the host (#9556). #1731: this was
/// the crate's third hand-rolled lock parser, and the panel would show an
/// address taken from any TOML file at that path.
/// Test: `client_lock_url_ignores_a_host_lock_under_isolation` (in
/// `trusty_mpm::core::discovery`); the record rules are covered in
/// `trusty_mpm::core::daemon_identity`.
fn read_lock_addr() -> Option<String> {
    // #9556: the filtered reader, so the banner and statusline never show the
    // host daemon from a sandbox.
    trusty_mpm::core::client_lock_url()
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
        // #6761: hermetic — this compat wrapper must not depend on whether a
        // console happens to be listening on the machine running the tests.
        console: ConsoleInfo::default(),
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

    /// Why (#9556 critic): a sandbox banner with no recorded console reports
    /// offline and names no address; it never dials the host's 7788.
    /// Test: itself.
    #[test]
    fn console_info_without_a_target_is_offline_and_unprobed() {
        let info = ConsoleInfo::probe(None);
        assert!(!info.online);
        assert!(info.addr.is_empty());
    }

    /// #6288: a hardened socket with a listener behind it reads online.
    #[tokio::test]
    async fn socket_probe_reports_a_listening_socket_online() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("s").join("trusty-mpm.sock");
        std::fs::create_dir_all(socket.parent().expect("parent")).expect("dir");
        let _listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        assert!(socket_probe(&socket));
    }

    /// #6288: an absent socket and a stale one (a file nothing serves) read
    /// offline.
    #[tokio::test]
    async fn socket_probe_reports_an_absent_or_stale_socket_offline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("s").join("trusty-mpm.sock");
        assert!(!socket_probe(&socket), "absent");
        std::fs::create_dir_all(socket.parent().expect("parent")).expect("dir");
        drop(trusty_common::uds::bind_hardened(&socket).expect("bind"));
        assert!(socket.exists(), "the stale socket file stays behind");
        assert!(!socket_probe(&socket), "stale");
    }

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
        // #6869: the row reports liveness; the port is not shown.
        assert!(
            out.contains("daemon  running"),
            "expected running label in daemon row"
        );
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
}
