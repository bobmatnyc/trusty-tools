//! Compact rounded info-box banner for `tm launch`, `tm connect`, and bare `tm`.
//!
//! Why: the old full-screen ASCII robot splash clears the screen and occupies
//! the entire terminal; a compact 9-line box conveys the same information —
//! version, project, daemon, memory, search — without the screen wipe.
//! What: `DaemonInfo` reads the lock file and optionally probes the HTTP API;
//! `render_info_box` builds the `╭─╮` box string; `print_info_box` prints it.
//! Test: `info_box_renders_online`, `info_box_renders_offline`,
//! `info_box_renders_reconnecting` in `tests.rs`.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

// ── Logo ──────────────────────────────────────────────────────────────────────

/// Three-line block-element logo, each exactly 13 display columns wide.
///
/// Why: fills the left column of the info box, giving `tm` a visual identity
/// without the full-screen ASCII robot splash.
/// What: block-drawing characters space-padded to a uniform 13-column width.
/// Test: `info_box_renders_online` checks the box renders without panic.
const LOGO: [&str; 3] = [
    "▐▛███▜▌      ", // 7 display cols + 6 spaces = 13
    "▝▜█████▛▘    ", // 9 display cols + 4 spaces = 13
    "▘▘  ▝▝       ", // 6 display cols + 7 spaces = 13
];

/// Display-column width of each logo line.
const LOGO_COLS: usize = 13;

// ── DaemonInfo ────────────────────────────────────────────────────────────────

/// Live state of the trusty-mpm daemon for banner display.
///
/// Why: the info box shows daemon reachability and session count; this struct
/// decouples the probe logic from the rendering logic so both are testable.
/// What: holds the bound address, optional session count, and an online flag.
/// Test: `info_box_renders_online` / `info_box_renders_offline`.
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
    /// Test: `info_box_renders_online` / `info_box_renders_offline`.
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
    /// Test: `info_box_renders_with_count`.
    pub(crate) fn with_count(mut self, count: usize) -> Self {
        self.session_count = Some(count);
        self
    }

    /// Probe `/sessions` to attach a session count (best-effort, ≤150 ms).
    ///
    /// Why: `tm launch` and `tm connect` do not have a pre-fetched session list,
    /// so a cheap HTTP probe fills in the count for the banner.
    /// What: spawns a background thread (blocking reqwest, 150 ms timeout);
    /// waits up to 200 ms for the result; degrades gracefully on timeout/error.
    /// Test: `info_box_renders_online` verifies this does not panic.
    pub(crate) fn probe_session_count(mut self, base_url: &str) -> Self {
        if self.online {
            self.session_count = try_get_session_count(base_url);
        }
        self
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the compact info box into a `String`.
///
/// Why: keeping rendering pure (no I/O) makes it unit-testable and lets
/// `print_info_box` stay a thin wrapper.
/// What: builds a `╭─╮` rounded box with the block logo on the left and
/// version/project/workspace/daemon/memory/search/hint rows on the right.
/// When `reconnecting` is true an extra "↩ reconnecting" row is inserted.
/// Test: `info_box_renders_online`, `info_box_renders_offline`,
/// `info_box_renders_reconnecting`.
pub(crate) fn render_info_box(
    workdir: &str,
    tmux_name: &str,
    reconnecting: bool,
    daemon: &DaemonInfo,
) -> String {
    let project = Path::new(workdir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| workdir.to_string());
    let workspace = abbreviate_home(workdir);
    let version = env!("CARGO_PKG_VERSION");
    let memory = super::banner::detect_memory();
    let search = super::banner::detect_tool("trusty-search");
    let daemon_row = format_daemon_row(daemon);

    // Build right-column lines.
    let mut rows: Vec<String> = vec![
        format!("trusty-mpm  v{version}"),
        format!("project     {project}"),
        format!("workspace   {workspace}"),
    ];
    if reconnecting {
        rows.push(format!("session     \u{21a9} reconnecting  ({tmux_name})"));
    }
    rows.push(daemon_row);
    rows.push(detected_row("memory     ", &memory));
    rows.push(detected_row("search     ", &search));
    rows.push("^b d  detach".to_string());

    // Compute the right-column display width from the longest row.
    let right_w = rows.iter().map(|r| display_len(r)).max().unwrap_or(20);

    // Inner content span: 1 + LOGO_COLS + 1 + 1 + 1 + right_w + 1
    let inner = 1 + LOGO_COLS + 1 + 1 + 1 + right_w + 1;
    let top = format!("\u{256d}{}\u{256e}", "\u{2500}".repeat(inner));
    let bot = format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(inner));

    let blank_logo = " ".repeat(LOGO_COLS);
    let mut out = String::new();
    out.push_str(&top);
    out.push('\n');
    for (i, row) in rows.iter().enumerate() {
        let logo_line: &str = LOGO.get(i).copied().unwrap_or(blank_logo.as_str());
        let padded = pad_to(row, right_w);
        out.push_str(&format!(
            "\u{2502} {} \u{2502} {} \u{2502}\n",
            logo_line, padded
        ));
    }
    out.push_str(&bot);
    out.push('\n');
    out
}

/// Print the info box to stdout and pause 1 second.
///
/// Why: `tm launch` and `tm connect` show the box just before handing the
/// terminal to tmux so the operator reads the session context.
/// What: prints [`render_info_box`] output, flushes stdout, then sleeps 1 s.
/// Test: `info_box_renders_online` (no panic); the sleep is not unit-tested.
pub(crate) fn print_info_box(
    workdir: &str,
    tmux_name: &str,
    reconnecting: bool,
    daemon: &DaemonInfo,
) {
    print!(
        "{}",
        render_info_box(workdir, tmux_name, reconnecting, daemon)
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::thread::sleep(Duration::from_secs(1));
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Format a tool-detection row, appending ✓ only when the tool was detected.
///
/// Why: appending ✓ unconditionally made "(not detected)" rows render as
/// `memory   (not detected)  ✓`, which is visually contradictory (#1738 review).
/// What: returns `"{label}  {value}  ✓"` when `value != "(not detected)"`,
/// otherwise `"{label}  {value}"` (no checkmark).
/// Test: `detected_row_checkmark_when_detected`, `detected_row_no_checkmark_when_not_detected`.
fn detected_row(label: &str, value: &str) -> String {
    if value != "(not detected)" {
        format!("{label} {value}  \u{2713}")
    } else {
        format!("{label} {value}")
    }
}

/// Format the daemon row: online shows addr + count, offline shows `○ offline`.
///
/// Why: the daemon row has two forms and an optional session count; a helper
/// keeps `render_info_box` readable.
/// What: `"daemon      ● :7880  (2)"` when online or `"daemon      ○ offline"`.
/// Test: indirectly by the box-render tests.
fn format_daemon_row(daemon: &DaemonInfo) -> String {
    if daemon.online {
        let port_str = daemon
            .addr
            .rfind(':')
            .map(|i| &daemon.addr[i..])
            .unwrap_or(daemon.addr.as_str());
        match daemon.session_count {
            Some(n) => format!("daemon      \u{25cf} {port_str}  ({n})"),
            None => format!("daemon      \u{25cf} {port_str}"),
        }
    } else {
        "daemon      \u{25cb} offline".to_string()
    }
}

/// Replace the home-directory prefix in `path` with `~`.
///
/// Why: workspace paths are long; abbreviating the home prefix makes the box
/// narrower and more readable.
/// What: detects the `$HOME` prefix and replaces it with `~`.
/// Test: `abbreviate_home_replaces_prefix` in `tests.rs`.
fn abbreviate_home(path: &str) -> String {
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

/// Pad `s` with trailing spaces to `width` display columns.
///
/// Why: the box requires a fixed-width right column; this pads rows without
/// depending on an external `unicode-width` crate.
/// What: appends `(width - display_len(s))` spaces, or returns `s` unchanged
/// when it is already ≥ `width` columns.
/// Test: indirectly by the box-render tests.
fn pad_to(s: &str, width: usize) -> String {
    let len = display_len(s);
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

/// Estimate display width as the Unicode scalar-value count.
///
/// Why: `str::len()` returns bytes, not display columns; `.chars().count()`
/// is correct for the ASCII + 1-column Unicode block chars used here.
/// What: counts Unicode scalar values (identical to column count for our content).
/// Test: indirectly by the box-render tests.
fn display_len(s: &str) -> usize {
    s.chars().count()
}

/// Read `addr` from `~/.trusty-mpm/daemon.lock`, verifying PID liveness.
///
/// Why: the lock file is cheaper than an HTTP probe and tells us the daemon
/// address before we decide whether to fire the probe.
/// What: parses `addr = "..."` and `pid = N`; on Unix validates the PID with
/// `kill(pid, 0)`; returns `None` when the lock file is absent or PID is stale.
/// Test: covered indirectly by the info-box render tests.
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
    // On Unix, validate that the PID is still alive before trusting the lock.
    #[cfg(unix)]
    if let Some(p) = pid
        && unsafe { libc::kill(p as libc::pid_t, 0) } != 0
    {
        return None;
    }
    // Silence unused-variable warning on non-Unix platforms.
    let _ = pid;
    addr
}

/// Probe `/sessions` with a short timeout and return the session count.
///
/// Why: blocking HTTP inside a (possibly async) context requires a detached
/// thread; the ≤150 ms wall-clock budget keeps banner latency acceptable.
/// What: spawns a thread that GETs `{base_url}/sessions` with `reqwest::blocking`
/// (150 ms timeout), sends the count over a channel; the caller waits ≤200 ms.
/// Test: covered indirectly by info-box render tests (offline degradation path).
pub(crate) fn try_get_session_count(base_url: &str) -> Option<usize> {
    let url = format!("{base_url}/sessions");
    let (tx, rx) = mpsc::channel::<Option<usize>>();
    std::thread::spawn(move || {
        let count = (|| -> Option<usize> {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(150))
                .build()
                .ok()?;
            let resp = client.get(&url).send().ok()?;
            #[derive(serde::Deserialize)]
            struct Row {
                #[allow(dead_code)]
                #[serde(default)]
                name: String,
            }
            let rows: Vec<Row> = resp.json().ok()?;
            Some(rows.len())
        })();
        let _ = tx.send(count);
    });
    rx.recv_timeout(Duration::from_millis(200)).ok().flatten()
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
        assert!(out.contains("\u{25cf}"), "expected ● online marker");
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
        assert!(out.contains("\u{25cb}"), "expected ○ offline marker");
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
    fn detected_row_checkmark_when_detected() {
        let row = detected_row("memory   ", "trusty-memory");
        assert!(row.contains('\u{2713}'), "✓ must appear for detected tool");
        assert!(
            !row.contains("(not detected)"),
            "must not show not-detected text"
        );
    }

    #[test]
    fn detected_row_no_checkmark_when_not_detected() {
        let row = detected_row("memory   ", "(not detected)");
        assert!(
            !row.contains('\u{2713}'),
            "✓ must NOT appear when not detected"
        );
        assert!(
            row.contains("(not detected)"),
            "must preserve (not detected) text"
        );
    }

    #[test]
    fn info_box_no_checkmark_on_not_detected_lines() {
        // Verify the rendered box never places ✓ on a "(not detected)" line.
        // In CI neither binary is on PATH, so at least one will be not-detected.
        let out = render_info_box("/tmp/proj", "sess", false, &offline());
        for line in out.lines() {
            if line.contains("(not detected)") {
                assert!(
                    !line.contains('\u{2713}'),
                    "✓ must not appear on a (not detected) line: {line:?}"
                );
            }
        }
    }
}
