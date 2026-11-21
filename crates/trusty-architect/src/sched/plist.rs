//! The launchd user-agent plist for the scheduler daemon.
//!
//! Why: one launchd agent keeps the scheduler alive (KeepAlive) without root.
//! `trusty-common` owns the shared renderer, but this crate is a leaf and
//! must not depend on it, so this follows its conventions by hand: the same
//! key order and values as its `render_plist` and the `com.trusty.events`
//! golden (`RunAtLoad`, `ThrottleInterval` 10, `ExitTimeOut` 60, fd limits,
//! log paths, a full `PATH`).
//! What: [`PlistSpec::render`] returns the XML. It is render-only: nothing
//! here writes `~/Library/LaunchAgents` or runs `launchctl`. The environment
//! holds `PATH` and `ARCHITECT_PROJECT_DIR`, never a credential. Rollback is
//! `launchctl bootout gui/$UID/com.trusty.architect-sched`, then delete the
//! plist and the binary.
//! Test: `plist_has_keepalive_label_and_program_arguments`,
//! `plist_escapes_xml_and_carries_no_credentials`.

use std::path::PathBuf;

/// The agent label (the `com.trusty.<name>` convention).
pub const LABEL: &str = "com.trusty.architect-sched";

/// What goes into the plist.
#[derive(Debug, Clone)]
pub struct PlistSpec {
    /// The installed binary, from `cargo install` (never `cp`).
    pub exe: PathBuf,
    /// The Architect project directory.
    pub root: PathBuf,
    /// Receives `stdout.log` and `stderr.log`.
    pub log_dir: PathBuf,
    /// The agent's `PATH`.
    pub path_env: String,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

impl PlistSpec {
    /// A `PATH` that covers Homebrew and the user bin directories launchd
    /// leaves out (`tmux`, `claude`, `tm`, `python3`).
    pub fn default_path(home: &std::path::Path) -> String {
        let h = home.display();
        format!(
            "{h}/.cargo/bin:{h}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        )
    }

    /// The plist XML.
    pub fn render(&self) -> String {
        let key = |k: &str, v: &str| format!("  <key>{k}</key>\n  {v}\n");
        let string = |s: &str| format!("<string>{}</string>", esc(s));
        let int = |n: u32| format!("<integer>{n}</integer>");
        let (exe, root) = (
            self.exe.display().to_string(),
            self.root.display().to_string(),
        );
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n",
        );
        s += &key("Label", &string(LABEL));
        s += "  <key>ProgramArguments</key>\n  <array>\n";
        for a in [exe.as_str(), "daemon", "--root", root.as_str()] {
            s += &format!("    {}\n", string(a));
        }
        s += "  </array>\n";
        s += &key("KeepAlive", "<true/>");
        s += &key("RunAtLoad", "<true/>");
        s += &key("ThrottleInterval", &int(10));
        s += &key("ExitTimeOut", &int(60));
        s += &key("WorkingDirectory", &string(&root));
        s += &key(
            "StandardOutPath",
            &string(&self.log_dir.join("stdout.log").display().to_string()),
        );
        s += &key(
            "StandardErrorPath",
            &string(&self.log_dir.join("stderr.log").display().to_string()),
        );
        for limit in ["SoftResourceLimits", "HardResourceLimits"] {
            s += &format!(
                "  <key>{limit}</key>\n  <dict>\n    <key>NumberOfFiles</key>\n    {}\n  </dict>\n",
                int(8192)
            );
        }
        s += "  <key>EnvironmentVariables</key>\n  <dict>\n";
        for (k, v) in [
            ("PATH", self.path_env.as_str()),
            ("ARCHITECT_PROJECT_DIR", root.as_str()),
        ] {
            s += &format!("    <key>{k}</key>\n    {}\n", string(v));
        }
        s += "  </dict>\n</dict>\n</plist>\n";
        s
    }
}
