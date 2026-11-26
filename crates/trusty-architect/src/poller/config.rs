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
use super::pyfmt::{py_float, py_int};
use super::steps::usage::UsagePaths;
use super::watchdog::{FROZEN_MIN_DEFAULT, SILENT_MINUTES_DEFAULT};
use super::{SKIP_SESSIONS, SUPERVISOR};
use crate::error::ConfigError;
use crate::install_paths::{self, InstallPathsError};
use crate::self_ctx::{default_dir, projects_dir};

/// Load alert line: the 15-minute load average over this many times the cores.
pub const LOAD_FACTOR_DEFAULT: f64 = 1.5;
/// `tm architect serve`'s least gap between event cycles, in seconds.
pub const SERVE_MIN_EVENT_GAP_DEFAULT: f64 = 15.0;
/// The context window a token count is divided by.
pub const SELF_CTX_WINDOW_DEFAULT: i64 = 1_000_000;
/// The Architect's own context percent that raises `self_ctx` with no
/// schedule bar and a 1M window (Bob 2026-11-05; was 25, ruling b7).
pub const SELF_CTX_THRESHOLD_DEFAULT: i64 = 35;

/// Where the `self_ctx` threshold comes from (df-T7i P1b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfCtxThreshold {
    /// `SELF_CTX_THRESHOLD` names the percent.
    Env(i64),
    /// Unset: the Architect's bar as a percent of the window
    /// ([`super::ctx_bar::bar_pct`], read from `il_schedule` each cycle).
    Bar,
}

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
    /// `SELF_CTX_THRESHOLD`, or the bar when unset. `None` when bad:
    /// `self_ctx` is then off.
    pub self_ctx_threshold: Option<SelfCtxThreshold>,
    /// `IL_SCHEDULE`, else `<root>/local/il-schedule.json`: the Architect's
    /// context bar and the sessions the completion check reads (df-T7i P1b).
    pub il_schedule: PathBuf,
    /// `SELF_CLEAR_SUPPRESS`, else `<inbox>/wake-suppress`: while this marker
    /// is fresh, self-clear waits for idle and no wake is typed (ar-T0c).
    pub wake_suppress: PathBuf,
    /// `PM_WAKE_ALERT`, else `<inbox>/pm-wake-alert.json`: the failed-wake
    /// record the Architect's hook reads (D39).
    pub wake_alert: PathBuf,
    /// `pm-messages.jsonl` in [`Config::read_inbox`]: the PM-message intake
    /// reads it, and held messages join the wake digest.
    pub pm_messages: PathBuf,
    /// `ARCHITECT_READ_INBOX_DIR`, else `ARCHITECT_INBOX_DIR`, else the inbox:
    /// where the read-only queues and `events.jsonl` live. The shadow points the inbox at a
    /// scratch directory and this at the live one, as a Python dry run reads
    /// the live inbox (MB1595).
    pub read_inbox: PathBuf,
    /// `TRIAGE_RULES`, else `<root>/local/triage-rules.json`: the holds
    /// [`super::RuleTriage`] reads.
    pub triage_rules: PathBuf,
    /// `SELF_REFRESH_PCT` (default 50): own context percent at which the
    /// poller starts `self-clear.sh`. `None` when bad: the step reports an
    /// error, as the prototype does.
    pub self_refresh_pct: Option<f64>,
    /// `USAGE_ZONE_FILE`, else `<root>/local/usage/zone-kalloc1024.jsonl`:
    /// the hourly kernel-zone log (D73).
    pub zone_log: PathBuf,
    /// `<read_inbox>/ctx-cycle-alerts.jsonl`: failures `ctx-cycle.py` queues.
    pub ctx_cycle_alerts: PathBuf,
    /// `<read_inbox>/selftest-state.json`: `runtime_selftest.py`'s result.
    pub selftest_state: PathBuf,
    /// `ARCHITECT_PYTHON`, else `python3`: runs the prototype's scripts.
    pub python: String,
    /// The script-step switches set to `off` (`steps::scripts::OFF_SWITCHES`).
    pub scripts_off: Vec<String>,
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
    /// The Architect project directory: a relaunched session starts there.
    pub root: PathBuf,
    /// `<root>/scripts/session-relaunch.sh`: the watchdog's relaunch (#9779).
    pub relaunch_script: PathBuf,
    /// `SILENT_MINUTES` (default 45; a bad value falls back, as in Python).
    pub silent_minutes: f64,
    /// `FROZEN_MIN` (default 15; a bad value falls back, as in Python).
    pub frozen_minutes: f64,
    /// `ARCHITECT_ROUTINES_FILE`, else `<root>/local/routines.json`: the D5r
    /// routine list. A missing file turns the check off.
    pub routines: PathBuf,
    /// `ARCHITECT_PUSH_CHANNEL`: where an overdue failure is pushed. Ships
    /// unset (nothing sent); `slack` sends a DM (D5r, D5t).
    pub push_channel: Option<String>,
    /// `ARCHITECT_PUSH_SLACK_USER`: the Slack user id the DM goes to (D5t).
    pub push_recipient: Option<String>,
    /// Shadow run: outputs go to `<inbox>/shadow/`, no lock, no keys, no
    /// relaunch, no notification, no mutating step (#9779, plan section 3).
    pub shadow: bool,
    /// `ARCHITECT_DRY_RUN_STATE`: a dry run's own state file, read and saved
    /// every cycle so a repeated dry run dedupes; unset, a dry run saves
    /// nothing. The scheduler's shadow poll sets it (#11602 follow-up).
    pub dry_run_state: Option<PathBuf>,
    /// `SERVE_MIN_EVENT_GAP` seconds, else [`SERVE_MIN_EVENT_GAP_DEFAULT`]:
    /// the least time between two event cycles of `tm architect serve`
    /// (#9779 review). The 60 s tick is unaffected.
    pub serve_min_gap: f64,
    /// tm's socket for the `dep_degraded` step (PR-B2): `TM_HEALTH_SOCKET`,
    /// else tm's own derivation under `home`; `None` when `TM_HEALTH=off`.
    pub tm_health_socket: Option<PathBuf>,
}

impl Config {
    /// Reads the configuration through `get` (e.g. `|k| std::env::var(k).ok()`).
    /// `root` is the Architect project directory; `home` is the user's home.
    /// The claude-config dir sits under the install's config dir
    /// ([`install_paths::config_dir`]), as tm's does (#11821 P4-f2).
    ///
    /// # Errors
    /// [`InstallPathsError`] when the config dir does not resolve: a broken or
    /// unreadable `install.toml`, or an empty or relative value, with no
    /// `$HOME` fallback. Every other bad value lands in [`Config::problems`].
    pub fn from_env(
        root: &Path,
        home: &Path,
        get: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, InstallPathsError> {
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
        // #11821: tm's claude-config dir, under the install's config dir.
        let base = install_paths::config_dir(home, get)?.path;
        let config = base.join(".trusty-tools/trusty-mpm/claude-config");
        let band_default = BandConfig::default();
        let lenient =
            |name: &str, default: f64| get(name).and_then(|v| py_float(&v)).unwrap_or(default);
        let root_text = root.to_string_lossy();
        let pane_ctx_window = int("PANE_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT);
        let self_ctx_window = int("SELF_CTX_WINDOW", SELF_CTX_WINDOW_DEFAULT);
        let self_ctx_threshold = match get("SELF_CTX_THRESHOLD") {
            None => Some(SelfCtxThreshold::Bar),
            Some(_) => int("SELF_CTX_THRESHOLD", 0).map(SelfCtxThreshold::Env),
        };
        let inbox = path("SUPERVISOR_INBOX_DIR", root.join("inbox"));
        // See MB1595: the read-only queues follow the live inbox in a shadow.
        let read_inbox = path(
            "ARCHITECT_READ_INBOX_DIR",
            path("ARCHITECT_INBOX_DIR", inbox.clone()),
        );
        // #9801: usage.py's paths, read as usage.py reads them.
        let claude_dir = path("CLAUDE_CONFIG_DIR", config.clone());
        let usage = UsagePaths {
            ledger: path("USAGE_LEDGER", root.join("local/usage/ledger.jsonl")),
            cache: path(
                "USAGE_STATUSLINE_CACHE",
                inbox.join("statusline-usage.json"),
            ),
            claude_json: path("USAGE_CLAUDE_JSON", claude_dir.join(".claude.json")),
            probe_script: root.join("scripts/usage-probe.sh"),
            probe_log: inbox.join("usage-probe.log"),
        };
        Ok(Self {
            inbox: inbox.clone(),
            sessions_dir: path("SESSIONS_DIR", config.join("sessions")),
            // #11821: the transcripts live under the same config dir.
            transcript_base: path("TRANSCRIPT_BASE", projects_dir(&base)),
            self_ctx_dir: path("SELF_CTX_DIR", default_dir(&base, &root_text)),
            tmux_socket: get("TMUX_SOCKET")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            load_factor,
            pane_ctx_window,
            self_ctx_window,
            self_ctx_threshold,
            il_schedule: path("IL_SCHEDULE", root.join("local/il-schedule.json")),
            wake_suppress: path("SELF_CLEAR_SUPPRESS", inbox.join("wake-suppress")),
            wake_alert: path("PM_WAKE_ALERT", inbox.join("pm-wake-alert.json")),
            pm_messages: read_inbox.join("pm-messages.jsonl"),
            self_refresh_pct: match get("SELF_REFRESH_PCT") {
                None => Some(super::self_refresh::SELF_REFRESH_PCT_DEFAULT),
                Some(v) => py_float(&v),
            },
            zone_log: path(
                "USAGE_ZONE_FILE",
                root.join("local/usage/zone-kalloc1024.jsonl"),
            ),
            ctx_cycle_alerts: read_inbox.join("ctx-cycle-alerts.jsonl"),
            selftest_state: read_inbox.join("selftest-state.json"),
            read_inbox,
            triage_rules: path("TRIAGE_RULES", root.join("local/triage-rules.json")),
            python: get("ARCHITECT_PYTHON")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "python3".to_owned()),
            scripts_off: super::steps::scripts::OFF_SWITCHES
                .iter()
                .filter(|n| get(n).as_deref() == Some("off"))
                .map(|n| (*n).to_owned())
                .collect(),
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
            root: root.to_owned(),
            relaunch_script: root.join("scripts/session-relaunch.sh"),
            silent_minutes: lenient("SILENT_MINUTES", SILENT_MINUTES_DEFAULT),
            frozen_minutes: lenient("FROZEN_MIN", FROZEN_MIN_DEFAULT),
            routines: path("ARCHITECT_ROUTINES_FILE", root.join("local/routines.json")),
            push_channel: get("ARCHITECT_PUSH_CHANNEL").filter(|v| !v.is_empty()),
            push_recipient: get("ARCHITECT_PUSH_SLACK_USER").filter(|v| !v.is_empty()),
            shadow: false,
            dry_run_state: get("ARCHITECT_DRY_RUN_STATE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            serve_min_gap: lenient("SERVE_MIN_EVENT_GAP", SERVE_MIN_EVENT_GAP_DEFAULT).max(0.0),
            tm_health_socket: (get("TM_HEALTH").as_deref() != Some("off")).then(|| {
                path(
                    "TM_HEALTH_SOCKET",
                    super::dep_health::tm_socket_path(home, get),
                )
            }),
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

    /// `<read_inbox>/events.jsonl`: hook events the poller turns into
    /// alerts; read-only, so a shadow reads the live file (MB1595).
    pub fn events(&self) -> PathBuf {
        self.read_inbox.join("events.jsonl")
    }

    /// `<inbox>/shadow`: every file a shadow run writes.
    pub fn shadow_dir(&self) -> PathBuf {
        self.inbox.join("shadow")
    }

    /// Where this run writes: the inbox, or [`Config::shadow_dir`].
    fn out_dir(&self) -> PathBuf {
        if self.shadow {
            self.shadow_dir()
        } else {
            self.inbox.clone()
        }
    }

    /// `<inbox>/alerts.jsonl`: the alert log the Architect reads.
    pub fn alerts(&self) -> PathBuf {
        self.out_dir().join("alerts.jsonl")
    }

    /// `<inbox>/poll-state.json`: dedup and sensor state across cycles.
    pub fn state(&self) -> PathBuf {
        self.out_dir().join("poll-state.json")
    }

    /// The state file a run reads: [`Config::dry_run_state`] in a dry run
    /// that names one, else [`Config::state`].
    pub fn state_read(&self, dry_run: bool) -> PathBuf {
        match &self.dry_run_state {
            Some(p) if dry_run => p.clone(),
            _ => self.state(),
        }
    }

    /// `<inbox>/poll.log` (`shadow/shadow.log` in a shadow run).
    pub fn log(&self) -> PathBuf {
        if self.shadow {
            self.shadow_dir().join("shadow.log")
        } else {
            self.inbox.join("poll.log")
        }
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
    fn config_reads_env_once_like_the_prototype() -> Result<(), InstallPathsError> {
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
        assert_eq!(
            (c.load_factor, c.self_ctx_threshold),
            (Some(1.5), Some(SelfCtxThreshold::Bar))
        );
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
    fn bad_ctx_variables_switch_off_their_sensor_and_are_reported() -> Result<(), InstallPathsError>
    {
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
