//! #9171 A3b-2: fixtures for the cycle and loop tests: a tempdir Architect
//! root, a quiet host, a fleet listing, and the `alerts.jsonl` reader.
//!
//! Every path lives under the tempdir; nothing touches a live inbox.
#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use trusty_architect::poller::steps::notify_macos::RecordingNotifier;
use trusty_architect::poller::steps::usage::RecordingProbe;
use trusty_architect::poller::{Config, DiskUsage, HostProbe, Runner, WakeAll};
use trusty_architect::{FakeClock, ScriptedTmux};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A tempdir Architect root and the config built over it.
pub struct Env {
    pub dir: TempDir,
    pub config: Config,
    /// #9801: the usage probe launcher `runner` uses; it records, never types.
    pub probe: RecordingProbe,
    /// #9801: the notifier `runner` uses; it records, never notifies.
    pub notifier: RecordingNotifier,
}

impl Env {
    /// The root with `vars` as the environment; the inbox exists, the
    /// self-ctx folder does not (no `self_ctx` alert).
    pub fn new(vars: &[(&str, &str)]) -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let config = config_at(dir.path(), vars)?;
        fs::create_dir_all(&config.inbox)?;
        fs::write(config.events(), "")?;
        Ok(Self {
            dir,
            config,
            probe: RecordingProbe::default(),
            notifier: RecordingNotifier::default(),
        })
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}

/// The config for an Architect root at `root`, every path under it.
pub fn config_at(root: &Path, vars: &[(&str, &str)]) -> TestResult<Config> {
    let base: Vec<(String, String)> = [
        ("SUPERVISOR_INBOX_DIR", "inbox"),
        ("SESSIONS_DIR", "sessions"),
        ("TRANSCRIPT_BASE", "projects"),
        ("SELF_CTX_DIR", "self"),
    ]
    .iter()
    .map(|(k, sub)| {
        (
            (*k).to_owned(),
            root.join(sub).to_string_lossy().into_owned(),
        )
    })
    .chain(vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
    .collect();
    let get = move |k: &str| {
        base.iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    Ok(Config::from_env(root, root, &get)?)
}

impl Env {
    /// Writes `<root>/local/quiet-sessions.txt`.
    pub fn quiet(&self, text: &str) -> io::Result<()> {
        fs::create_dir_all(self.root().join("local"))?;
        fs::write(&self.config.quiet_sessions, text)
    }

    /// Kinds written to `alerts.jsonl` for `session`, in file order.
    pub fn written(&self, session: &str) -> TestResult<Vec<String>> {
        let path = self.config.alerts();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for line in fs::read_to_string(path)?.lines() {
            let v: serde_json::Value = serde_json::from_str(line)?;
            if v["session"] == session {
                out.push(v["kind"].as_str().unwrap_or_default().to_owned());
            }
        }
        Ok(out)
    }

    /// Every `(session, pane, kind, fingerprint)` in `alerts.jsonl`.
    pub fn keys(&self) -> TestResult<Vec<String>> {
        let path = self.config.alerts();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for line in fs::read_to_string(path)?.lines() {
            let v: serde_json::Value = serde_json::from_str(line)?;
            out.push(format!(
                "{}|{}|{}|{}",
                v["session"], v["pane"], v["kind"], v["fingerprint"]
            ));
        }
        Ok(out)
    }

    pub fn log(&self) -> String {
        fs::read_to_string(self.config.log()).unwrap_or_default()
    }

    pub fn state_path(&self) -> PathBuf {
        self.config.state()
    }

    pub fn runner<'a>(
        &'a self,
        tmux: &'a ScriptedTmux,
        clock: &'a FakeClock,
        probe: &'a dyn HostProbe,
    ) -> Runner<'a, ScriptedTmux, FakeClock> {
        Runner {
            tmux,
            clock,
            probe,
            triage: &WakeAll,
            config: &self.config,
            usage_probe: &self.probe,
            notifier: &self.notifier,
            dry_run: false,
            messages: &trusty_architect::poller::NoMessages,
        }
    }
}

/// A host under every line: no load, disk, swap or band change after the
/// first band alert (session `""`).
pub struct QuietHost;

impl HostProbe for QuietHost {
    fn loadavg(&self) -> io::Result<[f64; 3]> {
        Ok([0.0; 3])
    }
    fn cores(&self) -> usize {
        16
    }
    fn data_volume(&self) -> PathBuf {
        "/data".into()
    }
    fn disk_usage(&self, _: &Path) -> io::Result<DiskUsage> {
        Ok(DiskUsage {
            total: 1 << 40,
            used: 1,
            free: 1 << 40,
        })
    }
    fn swap_used_mb(&self) -> io::Result<Option<f64>> {
        Ok(Some(0.0))
    }
    fn mem_free_pct(&self) -> Option<f64> {
        Some(60.0)
    }
}

/// A host whose load average cannot be read: the host check fails.
pub struct BrokenHost;

impl HostProbe for BrokenHost {
    fn loadavg(&self) -> io::Result<[f64; 3]> {
        Err(io::Error::other("getloadavg failed"))
    }
    fn cores(&self) -> usize {
        16
    }
    fn data_volume(&self) -> PathBuf {
        "/data".into()
    }
    fn disk_usage(&self, _: &Path) -> io::Result<DiskUsage> {
        Err(io::Error::other("statvfs failed"))
    }
    fn swap_used_mb(&self) -> io::Result<Option<f64>> {
        Ok(None)
    }
    fn mem_free_pct(&self) -> Option<f64> {
        None
    }
}

/// `list-panes` stdout for `(session, pane, cmd)` rows, all active.
pub fn listing(rows: &[(&str, &str, &str)]) -> String {
    rows.iter()
        .map(|(s, p, c)| format!("{s}\t{p}\t{c}\t11\t/w\t0\n"))
        .collect()
}

/// The prototype tests' `screen(PM_TURN, ctx=ctx)`: an empty input box under
/// a finished turn, with `ctx` on the status line.
pub fn screen(ctx: u32) -> String {
    let rule = "─".repeat(80);
    [
        "⏺ Merged #12 and the CI run is green.".to_owned(),
        String::new(),
        "  Next: the release notes.".to_owned(),
        rule.clone(),
        "❯\u{a0}".to_owned(),
        rule,
        format!(
            "  TM 1.7.4 ● | org/proj ⎇ tm-x | @me | Opus 5.5 | ctx {ctx}% | $1.00 | ⏳30% 📅40% | 💸1%/1%"
        ),
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 9 agents".to_owned(),
        format!("{}/rc", " ".repeat(70)),
        String::new(),
        "  ⏺ main".to_owned(),
    ]
    .join("\n")
        + "\n\n"
}

/// A tmux that lists `rows`, has no windows to rename, and captures `cap`
/// for every pane.
pub fn fleet(rows: &[(&str, &str, &str)], cap: &str) -> ScriptedTmux {
    ScriptedTmux::new()
        .fallback("list-panes", &listing(rows))
        .fallback("list-windows", "")
        .fallback("capture-pane", cap)
}
