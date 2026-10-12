//! Poller configuration: the prototype's module constants and environment
//! overrides, read once.
//!
//! Why: `fleet-poll.py` reads `os.environ` inside its sensors on every cycle,
//! so a test that sets one variable races every other test in the process. The
//! port reads the environment once through a caller-supplied lookup (#9171 A3a).
//! What: [`Config::from_env`] builds every path and threshold the A3a sensors
//! use. Paths fall back like `os.environ.get(X) or default`; numbers parse like
//! Python's `float()`/`int()`; a bad `LOAD_FACTOR`, `SELF_CTX_WINDOW`,
//! `SELF_CTX_THRESHOLD` or `PANE_CTX_WINDOW` is a [`ConfigError`] (the
//! prototype raises on each cycle), while a bad `BAND_*` value falls back to
//! its default (the prototype's `_env_float`).
//! Test: `config_reads_env_once_like_the_prototype`.

use std::path::{Path, PathBuf};

use super::band::BandConfig;
use super::pyfmt::{py_float, py_int};
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
    /// `LOAD_FACTOR`.
    pub load_factor: f64,
    /// `PANE_CTX_WINDOW`: the window a PM pane's transcript tokens divide by.
    pub pane_ctx_window: i64,
    /// `SELF_CTX_WINDOW`.
    pub self_ctx_window: i64,
    /// `SELF_CTX_THRESHOLD`.
    pub self_ctx_threshold: i64,
    /// The builder-band lines (`BAND_*`).
    pub band: BandConfig,
}

impl Config {
    /// Reads the configuration through `get` (e.g. `|k| std::env::var(k).ok()`).
    /// `root` is the Architect project directory; `home` is the user's home.
    ///
    /// # Errors
    /// [`ConfigError`] for a numeric variable Python's `float()`/`int()` rejects.
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
        let float = |name: &'static str, default: f64| match get(name) {
            None => Ok(default),
            Some(v) => py_float(&v).ok_or(ConfigError {
                name,
                value: v,
                want: "number",
            }),
        };
        let int = |name: &'static str, default: i64| match get(name) {
            None => Ok(default),
            Some(v) => py_int(&v).ok_or(ConfigError {
                name,
                value: v,
                want: "integer",
            }),
        };
        let config = home.join(".trusty-tools/trusty-mpm/claude-config");
        let band_default = BandConfig::default();
        let lenient =
            |name: &str, default: f64| get(name).and_then(|v| py_float(&v)).unwrap_or(default);
        let root_text = root.to_string_lossy();
        Ok(Self {
            inbox: path("SUPERVISOR_INBOX_DIR", root.join("inbox")),
            sessions_dir: path("SESSIONS_DIR", config.join("sessions")),
            transcript_base: path("TRANSCRIPT_BASE", projects_dir(home)),
            self_ctx_dir: path("SELF_CTX_DIR", default_dir(home, &root_text)),
            tmux_socket: get("TMUX_SOCKET")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            load_factor: float("LOAD_FACTOR", LOAD_FACTOR_DEFAULT)?,
            pane_ctx_window: int("PANE_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT)?,
            self_ctx_window: int("SELF_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT)?,
            self_ctx_threshold: int("SELF_CTX_THRESHOLD", SELF_CTX_THRESHOLD_DEFAULT)?,
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
        })
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
        assert_eq!((c.load_factor, c.self_ctx_threshold), (1.5, 25));
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
        assert_eq!(c.load_factor, 2.0);
        assert_eq!(c.band.stop_free_gib, 100.0, "_env_float falls back");
        assert_eq!(c.band.hold_free_gib, 150.0);

        let bad = |k: &str| (k == "SELF_CTX_WINDOW").then(|| "lots".to_owned());
        let err = Config::from_env(Path::new("/a"), Path::new("/h"), &bad);
        assert_eq!(
            err.err().map(|e| e.name),
            Some("SELF_CTX_WINDOW"),
            "Python's int() raises on it"
        );
        Ok(())
    }
}
