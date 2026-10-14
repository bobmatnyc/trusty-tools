//! Poller configuration: the prototype's module constants and environment
//! overrides, read once.
//!
//! Why: `fleet-poll.py` reads `os.environ` inside its sensors on every cycle,
//! so a test that sets one variable races every other test in the process. The
//! port reads the environment once through a caller-supplied lookup (#9171 A3a).
//! What: [`Config::from_env`] builds every path and threshold the A3a sensors
//! use. Paths fall back like `os.environ.get(X) or default`; numbers parse like
//! Python's `float()`/`int()`. A bad `LOAD_FACTOR`, `PANE_CTX_WINDOW`,
//! `SELF_CTX_WINDOW` or `SELF_CTX_THRESHOLD` builds the config anyway, turns
//! only the sensor that reads it off (the field is `None`) and lands in
//! [`Config::problems`] for the caller to log. For the ctx variables that is
//! the prototype's behaviour; for `LOAD_FACTOR` it is Architect ruling Q4
//! (#9171 A3b-2): the prototype's `host_alerts` raises on it and aborts the
//! whole cycle, the port turns off host alerts only. A bad `BAND_*` value
//! falls back to its default (the prototype's `_env_float`). #9801 adds
//! [`Config::usage`], the usage step's paths.
//! Test: `config_reads_env_once_like_the_prototype`,
//! `usage_paths_default_like_the_prototype` (tests/steps_cycle.rs),
//! `bad_ctx_variables_switch_off_their_sensor_and_are_reported`,
//! `bad_load_factor_disables_host_alerts_only`.

use std::path::{Path, PathBuf};

use super::band::BandConfig;
use super::steps::usage::UsagePaths;
use super::pyfmt::{py_float, py_int};
use super::{SKIP_SESSIONS, SUPERVISOR};
use crate::error::ConfigError;
use crate::self_ctx::{default_dir, projects_dir};

/// Load alert line: the 15-minute load average over this many times the cores.
pub const LOAD_FACTOR_DEFAULT: f64 = 1.5;
/// The context window a token count is divided by.
pub const SELF_CTX_WINDOW_DEFAULT: i64 = 1_000_000;
/// The Architect's own context percent that raises `self_ctx` (ruling b7).
pub const SELF_CTX_THRESHOLD_DEFAULT: i64 = 25;

/// Everything the A3a sensors read from the environment.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// `SUPERVISOR_INBOX_DIR`, else `<root>/inbox`.
    pub inbox: PathBuf,
    /// `SESSIONS_DIR`: Claude Code's live per-process session registry.
    pub sessions_dir: PathBuf,
    /// `TRANSCRIPT_BASE`: Claude Code's transcript folders, one per cwd.
    pub transcript_base: PathBuf,
    /// `SELF_CTX_DIR`: the Architect's own transcript folder.
    pub self_ctx_dir: PathBuf,
    /// `TMUX_SOCKET`, when set and non-empty.
    pub tmux_socket: Option<PathBuf>,
    /// `LOAD_FACTOR`. `None` when bad: host alerts are then off (ruling Q4).
    pub load_factor: Option<f64>,
    /// `PANE_CTX_WINDOW`: the window a PM pane's transcript tokens divide by.
    /// `None` when the value is not an integer: pane ctx is then off.
    pub pane_ctx_window: Option<i64>,
    /// `SELF_CTX_WINDOW`. `None` when bad: `self_ctx` is then off.
    pub self_ctx_window: Option<i64>,
    /// `SELF_CTX_THRESHOLD`. `None` when bad: `self_ctx` is then off.
    pub self_ctx_threshold: Option<i64>,
    /// Each bad `PANE_CTX_WINDOW` / `SELF_CTX_*` value, for the caller to log
    /// (the prototype logs `self_ctx ERROR ...` and `transcript ctx ERROR ...`).
    pub problems: Vec<ConfigError>,
    /// The builder-band lines (`BAND_*`).
    pub band: BandConfig,
    /// `<root>/local/quiet-sessions.txt`: sessions whose `idle` alerts drop.
    pub quiet_sessions: PathBuf,
    /// The Architect's session: the one pane the poller wakes. [`SUPERVISOR`]
    /// here; `tm architect poll` sets `ARCHITECT_SESSION` (#9822).
    pub supervisor: String,
    /// The poller's own session, never watched: `tm-architect-poll` here;
    /// `tm architect poll` sets `ARCHITECT_POLL_SESSION` (#9822).
    pub poll_session: String,
    /// The usage step's ledger, cache, account file and probe (#9801).
    pub usage: UsagePaths,
}

impl Config {
    /// Reads the configuration through `get` (e.g. `|k| std::env::var(k).ok()`).
    /// `root` is the Architect project directory; `home` is the user's home.
    ///
    /// # Errors
    /// None today: every bad value lands in [`Config::problems`]. The
    /// `Result` stays for a value that cannot fall back.
    pub fn from_env(
        root: &Path,
        home: &Path,
        get: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let path = |name: &str, default: PathBuf| {
            get(name)
                .filter(|v| !v.is_empty())
                .map_or(default, PathBuf::from)
        };
        let mut problems = Vec::new();
        // #9171 A3b-2 ruling Q4: a bad LOAD_FACTOR turns off host alerts only.
        let load_factor = match get("LOAD_FACTOR") {
            None => Some(LOAD_FACTOR_DEFAULT),
            Some(v) => py_float(&v).or_else(|| {
                problems.push(ConfigError {
                    name: "LOAD_FACTOR",
                    value: v,
                    want: "number",
                });
                None
            }),
        };
        let mut int = |name: &'static str, default: i64| match get(name) {
            None => Some(default),
            Some(v) => py_int(&v).or_else(|| {
                problems.push(ConfigError {
                    name,
                    value: v,
                    want: "integer",
                });
                None
            }),
        };
        let config = home.join(".trusty-tools/trusty-mpm/claude-config");
        let band_default = BandConfig::default();
        let lenient =
            |name: &str, default: f64| get(name).and_then(|v| py_float(&v)).unwrap_or(default);
        let root_text = root.to_string_lossy();
        let pane_ctx_window = int("PANE_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT);
        let self_ctx_window = int("SELF_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT);
        let self_ctx_threshold = int("SELF_CTX_THRESHOLD", SELF_CTX_THRESHOLD_DEFAULT);
        let inbox = path("SUPERVISOR_INBOX_DIR", root.join("inbox"));
        // #9801: usage.py's paths, read as usage.py reads them.
        let claude_dir = path("CLAUDE_CONFIG_DIR", config.clone());
        let usage = UsagePaths {
            ledger: path("USAGE_LEDGER", root.join("local/usage/ledger.jsonl")),
            cache: path("USAGE_STATUSLINE_CACHE", inbox.join("statusline-usage.json")),
            claude_json: path("USAGE_CLAUDE_JSON", claude_dir.join(".claude.json")),
            probe_script: root.join("scripts/usage-probe.sh"),
            probe_log: inbox.join("usage-probe.log"),
        };
        Ok(Self {
            inbox,
            sessions_dir: path("SESSIONS_DIR", config.join("sessions")),
            transcript_base: path("TRANSCRIPT_BASE", projects_dir(home)),
            self_ctx_dir: path("SELF_CTX_DIR", default_dir(home, &root_text)),
            tmux_socket: get("TMUX_SOCKET")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            load_factor,
            pane_ctx_window,
            self_ctx_window,
            self_ctx_threshold,
            problems,
            band: BandConfig {
                stop_load_factor: lenient("BAND_STOP_LOAD_FACTOR", band_default.stop_load_factor),
                hold_load_factor: lenient("BAND_HOLD_LOAD_FACTOR", band_default.hold_load_factor),
                open_load_factor: lenient("BAND_OPEN_LOAD_FACTOR", band_default.open_load_factor),
                rehold_load_factor: lenient(
                    "BAND_REHOLD_LOAD_FACTOR",
                    band_default.rehold_load_factor,
                ),
                stop_free_gib: lenient("BAND_STOP_FREE_GIB", band_default.stop_free_gib),
                hold_free_gib: lenient("BAND_HOLD_FREE_GIB", band_default.hold_free_gib),
                stop_mem_free_pct: lenient(
                    "BAND_STOP_MEM_FREE_PCT",
                    band_default.stop_mem_free_pct,
                ),
            },
            quiet_sessions: root.join("local").join("quiet-sessions.txt"),
            supervisor: SUPERVISOR.to_owned(),
            poll_session: format!("{SUPERVISOR}-poll"),
            usage,
        })
    }

    /// True for a session the poller never watches and whose events it drops:
    /// [`SKIP_SESSIONS`], plus this config's supervisor and poller sessions.
    ///
    /// Test: `a_configured_supervisor_is_skipped_and_the_only_pane_woken`.
    pub fn skips(&self, session: &str) -> bool {
        SKIP_SESSIONS.contains(&session)
            || session == self.supervisor
            || session == self.poll_session
    }

    /// `<inbox>/events.jsonl`: hook events the poller turns into alerts.
    pub fn events(&self) -> PathBuf {
        self.inbox.join("events.jsonl")
    }

    /// `<inbox>/alerts.jsonl`: the alert log the Architect reads.
    pub fn alerts(&self) -> PathBuf {
        self.inbox.join("alerts.jsonl")
    }

    /// `<inbox>/poll-state.json`: dedup and sensor state across cycles.
    pub fn state(&self) -> PathBuf {
        self.inbox.join("poll-state.json")
    }

    /// `<inbox>/poll.log`: one line per cycle and per error.
    pub fn log(&self) -> PathBuf {
        self.inbox.join("poll.log")
    }

    /// `<inbox>/poll.lock`: held by the one running poller (#9171 A3b-2).
    pub fn lock(&self) -> PathBuf {
        self.inbox.join("poll.lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_env_once_like_the_prototype() -> Result<(), ConfigError> {
        let none = |_: &str| None;
        let c = Config::from_env(Path::new("/arch"), Path::new("/home/u"), &none)?;
        assert_eq!(c.inbox, Path::new("/arch/inbox"));
        assert_eq!(c.state(), Path::new("/arch/inbox/poll-state.json"));
        assert_eq!(
            c.sessions_dir,
            Path::new("/home/u/.trusty-tools/trusty-mpm/claude-config/sessions")
        );
        assert_eq!(
            c.self_ctx_dir,
            Path::new("/home/u/.trusty-tools/trusty-mpm/claude-config/projects/-arch")
        );
        assert_eq!((c.load_factor, c.self_ctx_threshold), (Some(1.5), Some(25)));
        assert_eq!(
            c.quiet_sessions,
            Path::new("/arch/local/quiet-sessions.txt")
        );
        assert_eq!(c.lock(), Path::new("/arch/inbox/poll.lock"));
        assert_eq!(c.band, BandConfig::default());
        assert_eq!(c.tmux_socket, None);

        let set = |k: &str| {
            let v = match k {
                "SUPERVISOR_INBOX_DIR" => "",
                "TMUX_SOCKET" => "/s",
                "LOAD_FACTOR" => " 2 ",
                "BAND_STOP_FREE_GIB" => "junk",
                "BAND_HOLD_FREE_GIB" => "150",
                _ => return None,
            };
            Some(v.to_owned())
        };
        let c = Config::from_env(Path::new("/arch"), Path::new("/h"), &set)?;
        assert_eq!(c.inbox, Path::new("/arch/inbox"), "empty falls back");
        assert_eq!(c.tmux_socket.as_deref(), Some(Path::new("/s")));
        assert_eq!(c.load_factor, Some(2.0));
        assert_eq!(c.band.stop_free_gib, 100.0, "_env_float falls back");
        assert_eq!(c.band.hold_free_gib, 150.0);

        // #9171 ruling Q4: host alerts off, the config still builds.
        let bad = |k: &str| (k == "LOAD_FACTOR").then(|| "lots".to_owned());
        let c = Config::from_env(Path::new("/a"), Path::new("/h"), &bad)?;
        assert_eq!(c.load_factor, None);
        assert_eq!(
            c.problems.iter().map(|p| p.name).collect::<Vec<_>>(),
            ["LOAD_FACTOR"]
        );
        Ok(())
    }

    #[test]
    fn bad_ctx_variables_switch_off_their_sensor_and_are_reported() -> Result<(), ConfigError> {
        let bad = |k: &str| match k {
            "PANE_CTX_WINDOW" => Some(String::new()),
            "SELF_CTX_WINDOW" => Some("lots".to_owned()),
            "SELF_CTX_THRESHOLD" => Some("25%".to_owned()),
            "LOAD_FACTOR" => Some("2".to_owned()),
            _ => None,
        };
        let c = Config::from_env(Path::new("/a"), Path::new("/h"), &bad)?;
        assert_eq!(
            (c.pane_ctx_window, c.self_ctx_window, c.self_ctx_threshold),
            (None, None, None)
        );
        assert_eq!(c.load_factor, Some(2.0), "the other variables still apply");
        let names: Vec<_> = c
            .problems
            .iter()
            .map(|p| (p.name, p.value.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("PANE_CTX_WINDOW", ""),
                ("SELF_CTX_WINDOW", "lots"),
                ("SELF_CTX_THRESHOLD", "25%")
            ]
        );
        let none = |_: &str| None;
        let ok = Config::from_env(Path::new("/a"), Path::new("/h"), &none)?;
        assert!(ok.problems.is_empty());
        assert_eq!(ok.self_ctx_window, Some(1_000_000));
        Ok(())
    }
}
