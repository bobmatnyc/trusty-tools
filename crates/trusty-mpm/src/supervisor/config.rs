//! Supervisor configuration: poll cadence, auto-resume policy, idle classification.
//!
//! Why: the unattended supervisor must run with no live caller for hours, so its
//! behavior has to be driven entirely by *seed configuration* an operator sets
//! once (env vars / a struct) rather than by interactive prompts. Centralizing
//! the knobs in one struct keeps the env-var contract (`TRUSTY_MPM_AUTO_RESUME`,
//! `TRUSTY_MPM_SUPERVISOR_INTERVAL`, …) auditable and gives the loop a single
//! immutable config value to read each tick.
//! What: defines [`SupervisorConfig`] with the poll interval, the auto-resume
//! gate, the idle-classification toggle, and the metrics bind address; plus
//! [`SupervisorConfig::from_env`] which reads the documented env vars with safe
//! defaults.
//! Test: `config_defaults`, `auto_resume_env_parsing`, `interval_env_parsing`,
//! `classify_idle_env_parsing` in `super::tests`.

use std::net::SocketAddr;
use std::time::Duration;

/// Environment variable that gates auto-resume of `stopped` sessions.
///
/// Why: auto-resume is the supervisor's most consequential action — it brings a
/// runtime back without a human present — so it MUST be opt-in via an explicit
/// env var, matching the existing reconcile-on-boot gate in `DaemonState`.
/// What: the canonical name an operator sets to `1` / `true` to enable resume.
/// Test: `auto_resume_env_parsing`.
pub const ENV_AUTO_RESUME: &str = "TRUSTY_MPM_AUTO_RESUME";

/// Environment variable that overrides the poll interval (in seconds).
///
/// Why: overnight fleets want a slow cadence (cheap); active debugging wants a
/// fast one. An env override lets operators tune without recompiling.
/// What: parsed as `u64` seconds; invalid / absent values fall back to the default.
/// Test: `interval_env_parsing`.
pub const ENV_INTERVAL_SECS: &str = "TRUSTY_MPM_SUPERVISOR_INTERVAL";

/// Environment variable that toggles idle-session activity classification.
///
/// Why: classification calls the activity monitor (which may call an LLM). On a
/// host with no `OPENROUTER_API_KEY`, or when an operator wants zero token spend,
/// this lets them turn classification off while keeping auto-resume + metrics.
/// What: set to `0` / `false` to disable; defaults to enabled.
/// Test: `classify_idle_env_parsing`.
pub const ENV_CLASSIFY_IDLE: &str = "TRUSTY_MPM_SUPERVISOR_CLASSIFY";

/// Environment variable that overrides the metrics HTTP bind address.
///
/// Why: operators co-locating several daemons need to move the metrics port off
/// the default to avoid collisions.
/// What: parsed as a `SocketAddr`; invalid / absent values fall back to the default.
/// Test: `metrics_addr_env_parsing`.
pub const ENV_METRICS_ADDR: &str = "TRUSTY_MPM_SUPERVISOR_ADDR";

/// Default poll interval when [`ENV_INTERVAL_SECS`] is unset: 30 seconds.
///
/// Why: 30s balances responsiveness (a stopped session resumes within half a
/// minute) against load (the sweep lists sessions and may classify panes).
/// What: the fallback `Duration` used by [`SupervisorConfig::from_env`].
/// Test: `config_defaults`.
pub const DEFAULT_INTERVAL_SECS: u64 = 30;

/// Default metrics bind address when [`ENV_METRICS_ADDR`] is unset.
///
/// Why: loopback-only by default keeps fleet state off the network unless the
/// operator opts in; `7881` sits next to the daemon's `7880`.
/// What: the fallback address string parsed by [`SupervisorConfig::from_env`].
/// Test: `config_defaults`.
pub const DEFAULT_METRICS_ADDR: &str = "127.0.0.1:7881";

/// Immutable configuration for one supervisor run.
///
/// Why: the loop reads its policy from one value per tick; bundling the knobs in
/// a `Clone + Copy`-friendly struct (it is `Clone`; `SocketAddr` is `Copy`) makes
/// the supervisor trivially testable — a test constructs a config directly rather
/// than mutating process-wide env vars.
/// What: carries the poll [`Duration`], the `auto_resume` gate, the
/// `classify_idle` toggle, and the metrics [`SocketAddr`].
/// Test: `config_defaults` and every loop test constructs one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// How long to wait between fleet sweeps.
    pub interval: Duration,
    /// When `true`, `stopped` sessions are auto-resumed each sweep.
    pub auto_resume: bool,
    /// When `true`, idle `active` sessions have their pane classified.
    pub classify_idle: bool,
    /// Address the `/metrics` + `/health` HTTP server binds to.
    pub metrics_addr: SocketAddr,
}

impl Default for SupervisorConfig {
    /// Why: a sensible no-env default makes the type usable in tests and as a
    /// base to override; auto-resume defaults OFF (safety) and classification ON.
    /// What: 30s interval, `auto_resume = false`, `classify_idle = true`,
    /// metrics on `127.0.0.1:7881`.
    /// Test: `config_defaults`.
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            auto_resume: false,
            classify_idle: true,
            metrics_addr: DEFAULT_METRICS_ADDR
                .parse()
                .expect("DEFAULT_METRICS_ADDR is a valid SocketAddr literal"),
        }
    }
}

impl SupervisorConfig {
    /// Build a config from the documented environment variables.
    ///
    /// Why: the supervisor is launched unattended by launchd/systemd, which can
    /// only pass configuration through the environment; this is the single place
    /// that maps env vars onto the typed config.
    /// What: reads [`ENV_AUTO_RESUME`], [`ENV_INTERVAL_SECS`], [`ENV_CLASSIFY_IDLE`],
    /// and [`ENV_METRICS_ADDR`], each falling back to its default on absence or a
    /// parse failure.
    /// Test: `auto_resume_env_parsing`, `interval_env_parsing`,
    /// `classify_idle_env_parsing`, `metrics_addr_env_parsing`.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let auto_resume = env_bool(ENV_AUTO_RESUME).unwrap_or(defaults.auto_resume);
        let classify_idle = env_bool(ENV_CLASSIFY_IDLE).unwrap_or(defaults.classify_idle);
        let interval = std::env::var(ENV_INTERVAL_SECS)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .map(Duration::from_secs)
            .unwrap_or(defaults.interval);
        let metrics_addr = std::env::var(ENV_METRICS_ADDR)
            .ok()
            .and_then(|v| v.trim().parse::<SocketAddr>().ok())
            .unwrap_or(defaults.metrics_addr);
        Self {
            interval,
            auto_resume,
            classify_idle,
            metrics_addr,
        }
    }
}

/// Parse a boolean-ish environment variable.
///
/// Why: the env contract accepts `1`/`true`/`yes`/`on` (and their negatives) so
/// operators are not surprised by which spelling works; sharing one parser keeps
/// every boolean flag consistent.
/// What: returns `Some(true)` for truthy spellings, `Some(false)` for falsy ones,
/// and `None` when the var is unset or unrecognized (so the caller keeps its default).
/// Test: `env_bool_recognizes_truthy_and_falsy`.
fn env_bool(key: &str) -> Option<bool> {
    let raw = std::env::var(key).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
