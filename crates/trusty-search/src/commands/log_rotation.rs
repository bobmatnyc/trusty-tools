//! Log rotation for the launchd-managed `stderr.log` (issue #127).
//!
//! Why: `~/Library/Logs/trusty-search/stderr.log` is written directly by
//! launchd via the plist's `StandardErrorPath` key — the daemon never holds
//! the file handle, so in-process rotation is impossible. macOS ships
//! `newsyslog(8)` for exactly this, but its system config dirs
//! (`/etc/newsyslog.d/`) require root. To stay sudo-free we install a
//! *user-level* newsyslog config and a daily `LaunchAgent` that runs
//! `newsyslog -F -f <config>`; newsyslog only needs write access to the log
//! files themselves, which the user owns. SIGHUP is not required because
//! launchd reopens `StandardErrorPath` on the next write after the inode
//! changes, so rotation never interrupts the running daemon.
//! What: renders the newsyslog config + rotation LaunchAgent plist, resolves
//! their on-disk paths, and provides install + presence-check helpers used by
//! `trusty-search doctor` / `doctor --fix`.
//! Test: `cargo test --workspace` exercises the pure renderers and path
//! resolvers; `trusty-search doctor --fix` on macOS installs both files and a
//! follow-up `doctor` run reports the rotation check as OK.

#[cfg(target_os = "macos")]
use anyhow::Result;

/// Reverse-DNS label for the log-rotation LaunchAgent. A sub-unit of the
/// daemon's label, so the two agents are managed independently but named
/// together.
///
/// #4868: this restated the daemon's pre-fix label as a literal, so when that
/// label was wrong this one was wrong too — and stayed wrong after the daemon's
/// was corrected. The owner's host still has the resulting orphan,
/// `com.trusty.trusty-search.logrotate`, loaded with no main unit beside it;
/// it is recorded as a legacy alias so an install evicts it.
#[cfg(target_os = "macos")]
pub const ROTATION_LAUNCHD_LABEL: &str = trusty_common::launchd_labels::SEARCH_LOGROTATE;

/// Rotation policy constants (issue #127 acceptance criteria).
///
/// `SIZE_KB` — rotate once the log exceeds 1 MiB.
/// `KEEP` — retain at most 7 compressed archives.
/// Total on-disk footprint is therefore bounded at roughly
/// `1 MiB (current) + 7 × ~1 MiB (archives)` ≈ 8 MiB before gzip, and far
/// less once the archives are compressed.
#[cfg(target_os = "macos")]
pub const ROTATION_SIZE_KB: u32 = 1024;

/// Number of rotated archives to keep.
#[cfg(target_os = "macos")]
pub const ROTATION_KEEP: u32 = 7;

/// Resolve `~/Library/Logs/trusty-search/stderr.log` — the file launchd
/// writes the daemon's stderr to (see `service.rs::launchd_plist_body`).
#[cfg(target_os = "macos")]
pub fn stderr_log_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    Ok(home
        .join("Library")
        .join("Logs")
        .join("trusty-search")
        .join("stderr.log"))
}

/// Resolve the path of the user-level newsyslog config this tool installs.
///
/// Why: lives next to the daemon's other state under Application Support so a
/// `trusty-search service uninstall` style cleanup can find it, and so it is
/// never confused with a system `/etc/newsyslog.d/` entry.
#[cfg(target_os = "macos")]
pub fn newsyslog_conf_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    Ok(home
        .join("Library")
        .join("Application Support")
        .join("trusty-search")
        .join("newsyslog.conf"))
}

/// Resolve the path of the log-rotation LaunchAgent plist.
#[cfg(target_os = "macos")]
pub fn rotation_plist_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve $HOME"))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{ROTATION_LAUNCHD_LABEL}.plist")))
}

/// Render the newsyslog config body for the given `stderr.log` path.
///
/// Why: newsyslog's config is whitespace-delimited columns; building it via a
/// pure function keeps the format testable and documented in one place.
/// What: emits a single entry —
/// `<logfile> <mode> <count> <size> <when> <flags>` — that rotates at
/// `ROTATION_SIZE_KB`, keeps `ROTATION_KEEP` archives, also rotates daily
/// (`when = $D0`, midnight) so an idle daemon's log still ages out, and uses
/// flags `JN`: `J` = compress rotated archives with bzip2/gzip, `N` = do not
/// signal any process (launchd reopens the path on the next write, so no
/// SIGHUP is needed and there is no PID file to point at).
/// Test: `newsyslog_conf_body_has_expected_columns` asserts the size, count
/// and flag columns are present.
#[cfg(target_os = "macos")]
pub fn newsyslog_conf_body(stderr_log: &std::path::Path) -> String {
    format!(
        "# trusty-search log rotation (issue #127) — managed by `trusty-search doctor --fix`.\n\
         # Columns: logfile_name  mode  count  size  when  flags\n\
         # Rotates at {size} KB or daily (whichever comes first); keeps {keep} archives.\n\
         {path}    644  {keep}  {size}  $D0  JN\n",
        path = stderr_log.display(),
        size = ROTATION_SIZE_KB,
        keep = ROTATION_KEEP,
    )
}

/// Render the LaunchAgent plist that runs `newsyslog` against our config once
/// per day.
///
/// Why: a user cannot drop a file into `/etc/newsyslog.d/` without sudo, so we
/// schedule our own periodic `newsyslog -F -f <conf>` run. `-F` forces a
/// rotation check every run; `-f` points at the user-owned config. Running at
/// a fixed hour keeps the check predictable, and `StartCalendarInterval`
/// (rather than `StartInterval`) means a sleeping/offline Mac runs the job
/// once on next wake instead of accumulating missed ticks.
/// What: emits a minimal plist that invokes `/usr/sbin/newsyslog -F -f <conf>`
/// at 03:17 daily. The odd minute spreads load off the top of the hour.
/// Test: `rotation_plist_body_invokes_newsyslog` asserts the program args.
#[cfg(target_os = "macos")]
pub fn rotation_plist_body(newsyslog_conf: &std::path::Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/sbin/newsyslog</string>
        <string>-F</string>
        <string>-f</string>
        <string>{conf}</string>
    </array>
    <!-- Daily at 03:17. StartCalendarInterval (not StartInterval) so a Mac
         that was asleep at 03:17 runs the rotation once on next wake rather
         than firing repeatedly to "catch up". -->
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key>
        <integer>3</integer>
        <key>Minute</key>
        <integer>17</integer>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        label = ROTATION_LAUNCHD_LABEL,
        conf = newsyslog_conf.display(),
    )
}

/// True when log rotation is already configured for `stderr.log`.
///
/// Why: the doctor check and `--fix` both need a single source of truth for
/// "is rotation set up?". We treat rotation as configured when *either* a
/// system `/etc/newsyslog.d/trusty-search.conf` exists (operator installed it
/// with sudo) *or* our user-level config is present.
/// What: returns true if any known rotation config file exists on disk.
/// Test: `rotation_configured_false_when_nothing_installed` (uses a temp HOME
/// indirectly is impractical; covered by the doctor integration tests).
#[cfg(target_os = "macos")]
pub fn rotation_configured() -> bool {
    let system = std::path::Path::new("/etc/newsyslog.d/trusty-search.conf");
    if system.exists() {
        return true;
    }
    newsyslog_conf_path().map(|p| p.exists()).unwrap_or(false)
}

/// Install the user-level newsyslog config + rotation LaunchAgent.
///
/// Why: invoked by `trusty-search doctor --fix`. Keeps the side-effecting
/// install logic in one place so the doctor handler stays thin.
/// What: writes `newsyslog.conf`, writes the LaunchAgent plist, then
/// `bootout`s (ignoring errors) and `bootstrap`s the agent so it is scheduled
/// immediately and the `RunAtLoad` run performs a first rotation pass.
/// Test: covered by `doctor --fix` on macOS; unit-tested helpers render the
/// file bodies this function writes.
#[cfg(target_os = "macos")]
pub fn install_rotation() -> Result<()> {
    let stderr_log = stderr_log_path()?;
    let conf_path = newsyslog_conf_path()?;
    let plist_path = rotation_plist_path()?;

    if let Some(parent) = conf_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&conf_path, newsyslog_conf_body(&stderr_log))
        .map_err(|e| anyhow::anyhow!("write {}: {e}", conf_path.display()))?;

    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&plist_path, rotation_plist_body(&conf_path))
        .map_err(|e| anyhow::anyhow!("write {}: {e}", plist_path.display()))?;

    // (Re)load the LaunchAgent so the schedule takes effect and the
    // RunAtLoad pass rotates immediately if the log already exceeds 1 MB.
    let uid = nix::unistd::getuid().as_raw();
    let domain = format!("gui/{uid}");

    // #4868: evict the labels earlier installs of THIS agent registered. The
    // owner's host carries `com.trusty.trusty-search.logrotate` loaded with no
    // main unit beside it — bootstrapping the corrected label without booting
    // that out leaves two rotation jobs on one log file.
    for legacy in trusty_common::launchd_labels::legacy_labels_for(ROTATION_LAUNCHD_LABEL) {
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{legacy}")])
            .status();
        if let Some(stale) = plist_path
            .parent()
            .map(|p| p.join(format!("{legacy}.plist")))
        {
            let _ = std::fs::remove_file(stale);
        }
    }

    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain])
        .arg(&plist_path)
        .status();
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain])
        .arg(&plist_path)
        .status()
        .map_err(|e| anyhow::anyhow!("launchctl bootstrap failed: {e}"))?;
    if !status.success() {
        anyhow::bail!("launchctl bootstrap exited with {status}");
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn stderr_log_path_ends_with_expected_components() {
        let p = stderr_log_path().expect("HOME should resolve in tests");
        let s = p.to_string_lossy();
        assert!(s.ends_with("Library/Logs/trusty-search/stderr.log"), "{s}");
    }

    #[test]
    fn newsyslog_conf_path_under_application_support() {
        let p = newsyslog_conf_path().expect("HOME should resolve in tests");
        let s = p.to_string_lossy();
        assert!(
            s.ends_with("Library/Application Support/trusty-search/newsyslog.conf"),
            "{s}"
        );
    }

    #[test]
    fn rotation_plist_path_uses_rotation_label() {
        let p = rotation_plist_path().expect("HOME should resolve in tests");
        let s = p.to_string_lossy();
        assert!(s.contains(ROTATION_LAUNCHD_LABEL), "{s}");
        assert!(s.ends_with(".plist"), "{s}");
    }

    #[test]
    fn newsyslog_conf_body_has_expected_columns() {
        let log = std::path::Path::new("/Users/test/Library/Logs/trusty-search/stderr.log");
        let body = newsyslog_conf_body(log);
        // The data line carries the log path, keep count, size, and JN flags.
        assert!(body.contains("/Users/test/Library/Logs/trusty-search/stderr.log"));
        assert!(body.contains(&ROTATION_SIZE_KB.to_string()));
        assert!(body.contains(&format!("  {}  ", ROTATION_KEEP)));
        assert!(body.contains("$D0"), "should rotate daily as well: {body}");
        assert!(body.trim_end().ends_with("JN"), "flags column: {body}");
    }

    #[test]
    fn rotation_plist_body_invokes_newsyslog() {
        let conf = std::path::Path::new(
            "/Users/test/Library/Application Support/trusty-search/newsyslog.conf",
        );
        let body = rotation_plist_body(conf);
        assert!(body.contains("/usr/sbin/newsyslog"));
        assert!(body.contains("<string>-F</string>"));
        assert!(body.contains("<string>-f</string>"));
        assert!(body.contains(&conf.display().to_string()));
        assert!(body.contains(ROTATION_LAUNCHD_LABEL));
        assert!(body.contains("StartCalendarInterval"));
    }

    #[test]
    fn rotation_keep_count_bounds_disk_footprint() {
        // Acceptance criterion: at most 7 archives, rotate at 1 MB.
        assert_eq!(ROTATION_KEEP, 7);
        assert_eq!(ROTATION_SIZE_KB, 1024);
    }
}
