//! macOS launchd LaunchAgent generation and lifecycle management.
//!
//! Why: trusty-search, trusty-analyze, and trusty-memory each hand-rolled
//! launchd plist XML and shelled out to `launchctl` to install their daemons
//! as login agents. The three copies disagreed on how to obtain the user's
//! UID (one used the `nix` crate, one used `libc`, one parsed `id -u` output)
//! and on `KeepAlive` semantics. This module is the single shared
//! implementation.
//!
//! What: a [`LaunchdConfig`] struct that renders plist XML, installs it under
//! `~/Library/LaunchAgents`, and bootstraps/bootouts it via `launchctl`.
//! macOS-only — the whole module is gated behind `#[cfg(target_os = "macos")]`.
//!
//! Test: `render_plist` output is asserted in unit tests (pure string
//! generation). `install`/`bootstrap`/`bootout` shell out and are exercised
//! manually by downstream `setup` commands.
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

/// Restart policy for a launchd agent.
///
/// Why: trusty-* daemons want to be restarted automatically, but the two
/// useful policies differ — "always keep alive" versus "restart only when the
/// process exits non-zero". Encoding the choice as an enum prevents callers
/// from hand-writing the wrong `KeepAlive` plist fragment.
/// What: [`KeepAlive::Always`] renders `<key>KeepAlive</key><true/>`;
/// [`KeepAlive::OnSuccess`] renders a dictionary with `SuccessfulExit` set to
/// `false`, telling launchd to restart unless the process exited 0.
/// Test: both variants are covered by `render_plist_*` unit tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepAlive {
    /// Keep the process running at all times; restart immediately on exit.
    Always,
    /// Restart only when the process exits non-zero (`SuccessfulExit: false`).
    OnSuccess,
}

/// File-descriptor ceiling emitted into every generated LaunchAgent plist.
///
/// Why: macOS launchd's default soft fd limit for user agents is 256. The
/// trusty-memory daemon opens ~3 redb files per palace (data, KG, vector
/// index) plus sockets, log descriptors, and reqwest connection pools.
/// At 82 palaces that is ~246 fds — right at the ceiling. When the limit is
/// exhausted, `open()` returns EMFILE, palace handles become non-functional,
/// and launchd's `KeepAlive` respawns each crashing instance into a zombie
/// herd (69 observed in the wild) because each new instance fails to bind
/// on the already-occupied port and exits non-zero. 8192 provides headroom
/// for thousands of palaces and matches the `ulimit -n 8192` hand-patch that
/// mitigated the live incident; setting both soft and hard to the same value
/// avoids any unexpected launchd-imposed ceiling below the hard limit.
/// What: the integer rendered into both `SoftResourceLimits` and
/// `HardResourceLimits` dictionaries' `NumberOfFiles` key.
/// Test: `render_plist_includes_resource_limits` asserts both dicts are present
/// with this value.
pub const LAUNCHD_FD_LIMIT: u32 = 8192;

/// Declarative description of a launchd LaunchAgent.
///
/// Why: assembling a plist by hand is error-prone (XML escaping, key ordering,
/// log-path wiring). A struct with a `render_plist` method makes the agent
/// definition data, not code, so every trusty-* setup command produces an
/// identical, correct plist.
/// What: holds the agent label, executable path and args, log directory,
/// restart policy, throttle interval, environment variables, and the optional
/// fd-limit override (defaults to [`LAUNCHD_FD_LIMIT`] for new agents).
/// Methods turn it into XML and drive `launchctl`.
/// Test: `render_plist` output asserted in unit tests; `plist_path` checked
/// against the expected `~/Library/LaunchAgents/<label>.plist` layout.
#[derive(Debug, Clone)]
pub struct LaunchdConfig {
    /// Reverse-DNS-style agent label, e.g. `com.trusty.search`. Also the
    /// plist file's base name.
    pub label: String,
    /// Absolute path to the daemon executable.
    pub exe_path: PathBuf,
    /// Arguments passed to the executable (after `argv[0]`).
    pub args: Vec<String>,
    /// Directory for log files: stdout → `<log_dir>/stdout.log`,
    /// stderr → `<log_dir>/stderr.log`.
    pub log_dir: PathBuf,
    /// Restart policy.
    pub keep_alive: KeepAlive,
    /// Minimum seconds between successive launches (launchd `ThrottleInterval`).
    pub throttle_interval: u32,
    /// Extra environment variables for the daemon process.
    pub env_vars: Vec<(String, String)>,
    /// `NumberOfFiles` written into both `SoftResourceLimits` and
    /// `HardResourceLimits` plist dicts. `None` suppresses both dicts
    /// (useful for agents that do not open many files). New agents should
    /// leave this as [`Some(LAUNCHD_FD_LIMIT)`] (the default via
    /// [`LaunchdConfig::new`]).
    pub fd_limit: Option<u32>,
}

impl LaunchdConfig {
    /// Render the launchd plist XML for this agent.
    ///
    /// Why: launchd consumes a property-list XML document; generating it in one
    /// audited place avoids the escaping and key-ordering bugs that plagued the
    /// three sibling implementations.
    /// What: returns a complete plist `String` with `Label`, `ProgramArguments`
    /// (exe + args), `KeepAlive` (per [`KeepAlive`]), `ThrottleInterval`,
    /// `RunAtLoad`, `StandardOutPath`/`StandardErrorPath` under `log_dir`,
    /// `SoftResourceLimits` + `HardResourceLimits` dicts (when `fd_limit` is
    /// `Some`), and an `EnvironmentVariables` dictionary when `env_vars` is
    /// non-empty. All string values are XML-escaped.
    /// Test: `render_plist_contains_core_keys`, `render_plist_keepalive_*`,
    /// `render_plist_escapes_xml`, `render_plist_includes_resource_limits`.
    pub fn render_plist(&self) -> Result<String> {
        let exe = self
            .exe_path
            .to_str()
            .context("exe_path is not valid UTF-8")?;
        let stdout = self.log_dir.join("stdout.log");
        let stderr = self.log_dir.join("stderr.log");
        let stdout = stdout
            .to_str()
            .context("stdout log path is not valid UTF-8")?;
        let stderr = stderr
            .to_str()
            .context("stderr log path is not valid UTF-8")?;

        let mut s = String::new();
        s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        s.push_str(
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        );
        s.push_str("<plist version=\"1.0\">\n");
        s.push_str("<dict>\n");

        s.push_str("  <key>Label</key>\n");
        s.push_str(&format!("  <string>{}</string>\n", xml_escape(&self.label)));

        s.push_str("  <key>ProgramArguments</key>\n");
        s.push_str("  <array>\n");
        s.push_str(&format!("    <string>{}</string>\n", xml_escape(exe)));
        for arg in &self.args {
            s.push_str(&format!("    <string>{}</string>\n", xml_escape(arg)));
        }
        s.push_str("  </array>\n");

        s.push_str("  <key>KeepAlive</key>\n");
        match self.keep_alive {
            KeepAlive::Always => s.push_str("  <true/>\n"),
            KeepAlive::OnSuccess => {
                s.push_str("  <dict>\n");
                s.push_str("    <key>SuccessfulExit</key>\n");
                s.push_str("    <false/>\n");
                s.push_str("  </dict>\n");
            }
        }

        s.push_str("  <key>RunAtLoad</key>\n");
        s.push_str("  <true/>\n");

        s.push_str("  <key>ThrottleInterval</key>\n");
        s.push_str(&format!(
            "  <integer>{}</integer>\n",
            self.throttle_interval
        ));

        s.push_str("  <key>StandardOutPath</key>\n");
        s.push_str(&format!("  <string>{}</string>\n", xml_escape(stdout)));
        s.push_str("  <key>StandardErrorPath</key>\n");
        s.push_str(&format!("  <string>{}</string>\n", xml_escape(stderr)));

        // Soft + hard fd limits — both dicts must be present so the hard
        // ceiling matches the soft request and launchd cannot silently clamp
        // the soft limit below what we asked for.
        if let Some(fd) = self.fd_limit {
            for key in &["SoftResourceLimits", "HardResourceLimits"] {
                s.push_str(&format!("  <key>{key}</key>\n"));
                s.push_str("  <dict>\n");
                s.push_str("    <key>NumberOfFiles</key>\n");
                s.push_str(&format!("    <integer>{fd}</integer>\n"));
                s.push_str("  </dict>\n");
            }
        }

        if !self.env_vars.is_empty() {
            s.push_str("  <key>EnvironmentVariables</key>\n");
            s.push_str("  <dict>\n");
            for (k, v) in &self.env_vars {
                s.push_str(&format!("    <key>{}</key>\n", xml_escape(k)));
                s.push_str(&format!("    <string>{}</string>\n", xml_escape(v)));
            }
            s.push_str("  </dict>\n");
        }

        s.push_str("</dict>\n");
        s.push_str("</plist>\n");
        Ok(s)
    }

    /// Return the install path for this agent's plist:
    /// `~/Library/LaunchAgents/<label>.plist`.
    ///
    /// Why: launchd discovers per-user agents in `~/Library/LaunchAgents`.
    /// Centralising the path keeps install and uninstall in agreement.
    /// What: joins the user's home directory with
    /// `Library/LaunchAgents/<label>.plist`. Errors if the home directory
    /// cannot be resolved.
    /// Test: `plist_path_layout`.
    pub fn plist_path(&self) -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not resolve home directory")?;
        Ok(home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", self.label)))
    }

    /// Install the agent: write the plist and ensure the log directory exists.
    ///
    /// Why: `launchctl bootstrap` fails if the plist file or the log directory
    /// it references is missing. Doing both in one call removes a footgun.
    /// What: creates `~/Library/LaunchAgents` and `log_dir` if absent, then
    /// writes the rendered plist to [`plist_path`](Self::plist_path). Does not
    /// load the agent — call [`bootstrap`](Self::bootstrap) for that.
    /// Test: side-effecting filesystem write; exercised by downstream `setup`.
    pub fn install(&self) -> Result<()> {
        let plist = self.plist_path()?;
        if let Some(parent) = plist.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create LaunchAgents dir {}", parent.display()))?;
        }
        std::fs::create_dir_all(&self.log_dir)
            .with_context(|| format!("create log dir {}", self.log_dir.display()))?;
        let xml = self.render_plist()?;
        std::fs::write(&plist, xml).with_context(|| format!("write plist {}", plist.display()))?;
        Ok(())
    }

    /// Bootstrap (load and start) the agent via `launchctl`, idempotently.
    ///
    /// Why: `launchctl bootstrap` errors if the agent is already loaded.
    /// Re-running a `setup` command must not fail for that reason, so we
    /// bootout first and ignore the "not loaded" error.
    /// What: calls [`bootout`](Self::bootout) (ignoring its result), then runs
    /// `launchctl bootstrap gui/<uid> <plist_path>`. Requires the plist to
    /// already be installed via [`install`](Self::install).
    /// Test: side-effecting `launchctl` call; exercised manually.
    pub fn bootstrap(&self) -> Result<()> {
        // Best-effort unload first so bootstrap doesn't trip on an existing
        // registration.
        let _ = self.bootout();

        let plist = self.plist_path()?;
        let domain = format!("gui/{}", current_uid());
        let output = Command::new("launchctl")
            .arg("bootstrap")
            .arg(&domain)
            .arg(&plist)
            .output()
            .context("spawn launchctl bootstrap")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "launchctl bootstrap {} {} failed: {}",
                domain,
                plist.display(),
                stderr.trim()
            );
        }
        Ok(())
    }

    /// Bootout (stop and unload) the agent via `launchctl`.
    ///
    /// Why: uninstalling or re-bootstrapping an agent requires removing the
    /// existing registration. A not-loaded agent is not an error condition for
    /// callers, so that case returns `Ok(())`.
    /// What: runs `launchctl bootout gui/<uid>/<label>`. Returns `Ok(())` on
    /// success or when `launchctl` reports the agent is not loaded; any other
    /// failure propagates as `Err`.
    /// Test: side-effecting `launchctl` call; exercised manually.
    pub fn bootout(&self) -> Result<()> {
        let domain_target = format!("gui/{}/{}", current_uid(), self.label);
        let output = Command::new("launchctl")
            .arg("bootout")
            .arg(&domain_target)
            .output()
            .context("spawn launchctl bootout")?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        // launchctl reports a not-loaded agent in a few phrasings / exit codes.
        if stderr.contains("no such process")
            || stderr.contains("not find")
            || stderr.contains("not loaded")
            || stderr.contains("could not find specified service")
        {
            return Ok(());
        }
        anyhow::bail!(
            "launchctl bootout {} failed: {}",
            domain_target,
            stderr.trim()
        );
    }
}

/// Return the current process's real user ID.
///
/// Why: launchd's per-user GUI domain is addressed as `gui/<uid>`. The three
/// sibling projects each resolved the UID differently (`nix`, `libc`, shelling
/// out to `id -u`); this is the single shared, dependency-light answer.
/// What: returns `libc::getuid()`. The call is always safe — `getuid` cannot
/// fail and has no preconditions.
/// Test: `current_uid_is_nonzero_for_normal_user` sanity-checks the value is
/// plausible (developers don't run the test suite as root).
pub fn current_uid() -> u32 {
    // SAFETY: getuid() is a POSIX call with no arguments and no failure mode.
    unsafe { libc::getuid() }
}

/// Escape a string for safe inclusion in plist XML text content.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(keep_alive: KeepAlive) -> LaunchdConfig {
        LaunchdConfig {
            label: "com.trusty.search".to_string(),
            exe_path: PathBuf::from("/usr/local/bin/trusty-search"),
            args: vec![
                "daemon".to_string(),
                "--port".to_string(),
                "7777".to_string(),
            ],
            log_dir: PathBuf::from("/Users/test/Library/Logs/trusty-search"),
            keep_alive,
            throttle_interval: 10,
            env_vars: vec![],
            fd_limit: Some(LAUNCHD_FD_LIMIT),
        }
    }

    #[test]
    fn render_plist_contains_core_keys() {
        let xml = sample(KeepAlive::Always).render_plist().unwrap();
        assert!(xml.contains("<key>Label</key>"));
        assert!(xml.contains("<string>com.trusty.search</string>"));
        assert!(xml.contains("<string>/usr/local/bin/trusty-search</string>"));
        assert!(xml.contains("<string>daemon</string>"));
        assert!(xml.contains("<key>ThrottleInterval</key>"));
        assert!(xml.contains("<integer>10</integer>"));
        assert!(xml.contains("<key>RunAtLoad</key>"));
        assert!(xml.contains("stdout.log"));
        assert!(xml.contains("stderr.log"));
        assert!(xml.trim_end().ends_with("</plist>"));
    }

    #[test]
    fn render_plist_keepalive_always() {
        let xml = sample(KeepAlive::Always).render_plist().unwrap();
        assert!(xml.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(!xml.contains("SuccessfulExit"));
    }

    #[test]
    fn render_plist_keepalive_on_success() {
        let xml = sample(KeepAlive::OnSuccess).render_plist().unwrap();
        assert!(xml.contains("<key>SuccessfulExit</key>"));
        assert!(xml.contains("<false/>"));
    }

    #[test]
    fn render_plist_includes_env_vars() {
        let mut cfg = sample(KeepAlive::Always);
        cfg.env_vars = vec![("RUST_LOG".to_string(), "info".to_string())];
        let xml = cfg.render_plist().unwrap();
        assert!(xml.contains("<key>EnvironmentVariables</key>"));
        assert!(xml.contains("<key>RUST_LOG</key>"));
        assert!(xml.contains("<string>info</string>"));
    }

    #[test]
    fn render_plist_omits_env_block_when_empty() {
        let xml = sample(KeepAlive::Always).render_plist().unwrap();
        assert!(!xml.contains("EnvironmentVariables"));
    }

    /// Why: The fd-exhaustion bug (246 open fds for 82 palaces against a 256
    /// soft limit) required a manual plist hand-patch to survive. This test
    /// asserts that every generated plist carries both `SoftResourceLimits`
    /// and `HardResourceLimits` with the canonical value so the fix survives
    /// refactors and `service start` / `service install` regenerations.
    /// What: renders a plist with `fd_limit = Some(LAUNCHD_FD_LIMIT)` and
    /// asserts both resource-limit dicts and the integer value appear. Also
    /// asserts that `fd_limit = None` suppresses both dicts.
    /// Test: itself.
    #[test]
    fn render_plist_includes_resource_limits() {
        let xml = sample(KeepAlive::OnSuccess).render_plist().unwrap();
        assert!(
            xml.contains("<key>SoftResourceLimits</key>"),
            "SoftResourceLimits must be present"
        );
        assert!(
            xml.contains("<key>HardResourceLimits</key>"),
            "HardResourceLimits must be present"
        );
        assert!(
            xml.contains(&format!("<integer>{LAUNCHD_FD_LIMIT}</integer>")),
            "NumberOfFiles must equal LAUNCHD_FD_LIMIT ({LAUNCHD_FD_LIMIT})"
        );

        // When fd_limit is None, neither dict should appear.
        let mut cfg = sample(KeepAlive::Always);
        cfg.fd_limit = None;
        let xml_no_limits = cfg.render_plist().unwrap();
        assert!(
            !xml_no_limits.contains("SoftResourceLimits"),
            "fd_limit=None must suppress SoftResourceLimits"
        );
        assert!(
            !xml_no_limits.contains("HardResourceLimits"),
            "fd_limit=None must suppress HardResourceLimits"
        );
    }

    #[test]
    fn render_plist_escapes_xml() {
        let mut cfg = sample(KeepAlive::Always);
        cfg.args = vec!["--name".to_string(), "a&b<c>\"d\"".to_string()];
        let xml = cfg.render_plist().unwrap();
        assert!(xml.contains("a&amp;b&lt;c&gt;&quot;d&quot;"));
        assert!(!xml.contains("a&b<c>"));
    }

    #[test]
    fn plist_path_layout() {
        let p = sample(KeepAlive::Always).plist_path().unwrap();
        assert!(p.ends_with("Library/LaunchAgents/com.trusty.search.plist"));
    }

    #[test]
    fn current_uid_is_nonzero_for_normal_user() {
        // CI and developer machines do not run tests as root.
        assert_ne!(current_uid(), 0, "test suite should not run as root");
    }

    #[test]
    fn xml_escape_handles_all_entities() {
        assert_eq!(xml_escape("&<>\"'"), "&amp;&lt;&gt;&quot;&apos;");
        assert_eq!(xml_escape("plain"), "plain");
    }
}
