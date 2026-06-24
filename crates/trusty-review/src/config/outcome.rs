//! Outcome-polling configuration (issue #1421).
//!
//! Why: outcome tracking (reactions + follow-up commits → suppression list) must
//! be opt-in (disabled by default) and its polling delay and dismissal threshold
//! must be independently tunable, matching the rest of the config module's
//! env-over-file-over-default pattern.
//!
//! What: exposes `OutcomeConfig` (resolved) and its TOML-deserialisable mirror
//! `OutcomeFileConfig`.  `from_env_and_file` resolves the two-layer precedence.
//!
//! Test: `outcome_defaults_disabled`, `outcome_env_enables`,
//! `outcome_file_configures`.

use serde::Deserialize;
use tracing::warn;

const ENV_OUTCOME_ENABLED: &str = "TRUSTY_REVIEW_OUTCOME_ENABLED";
const ENV_OUTCOME_POLL_DELAY: &str = "TRUSTY_REVIEW_OUTCOME_POLL_DELAY_MINUTES";
const ENV_OUTCOME_THRESHOLD: &str = "TRUSTY_REVIEW_OUTCOME_DISMISSAL_THRESHOLD";

/// Resolved configuration for the outcome-polling pipeline.
///
/// Why: the webhook handler and the background poll task both read these flags;
/// a single owned struct keeps the decision logic free of scattered env lookups
/// and makes the behaviour trivially testable (construct the struct directly).
/// What: `enabled` gates whether the outcome poll is scheduled on PR close;
/// `poll_delay_minutes` is how long to wait after merge before polling;
/// `dismissal_threshold` is the minimum dismissed-count for suppression-list inclusion.
/// Test: `outcome_defaults_disabled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeConfig {
    /// When `true`, outcome polling is scheduled on PR closed+merged events.
    /// Default: `false` (opt-in).
    pub enabled: bool,
    /// How many minutes to wait after a PR merge before polling reactions/commits.
    /// Default: `60`.
    pub poll_delay_minutes: u64,
    /// Minimum dismiss count for a finding kind to appear in `dismissed_patterns`.
    /// Default: `5`.
    pub dismissal_threshold: u32,
}

impl Default for OutcomeConfig {
    /// Why: outcome polling defaults to disabled so new deployments do not
    /// unexpectedly schedule background tasks or write to the outcome store.
    /// What: `enabled=false`, `poll_delay_minutes=60`, `dismissal_threshold=5`.
    /// Test: `outcome_defaults_disabled`.
    fn default() -> Self {
        Self {
            enabled: false,
            poll_delay_minutes: 60,
            dismissal_threshold: 5,
        }
    }
}

impl OutcomeConfig {
    /// Resolve from env vars layered over an optional `[outcome]` TOML table.
    ///
    /// Why: matches the rest of the config module's env-over-file-over-default
    /// precedence so operators have one mental model for every knob.
    /// What: starts from the file value (or default), then applies env overrides.
    /// Unrecognised env values emit a warning and leave the file/default unchanged.
    /// Test: `outcome_env_enables`, `outcome_file_configures`.
    pub fn from_env_and_file(file: Option<&OutcomeFileConfig>) -> Self {
        let mut cfg = OutcomeConfig {
            enabled: file.and_then(|f| f.enabled).unwrap_or(false),
            poll_delay_minutes: file.and_then(|f| f.poll_delay_minutes).unwrap_or(60),
            dismissal_threshold: file.and_then(|f| f.dismissal_threshold).unwrap_or(5),
        };
        if let Some(v) = parse_bool_env(ENV_OUTCOME_ENABLED) {
            cfg.enabled = v;
        }
        if let Some(v) = parse_u64_env(ENV_OUTCOME_POLL_DELAY) {
            cfg.poll_delay_minutes = v;
        }
        if let Some(v) = parse_u32_env(ENV_OUTCOME_THRESHOLD) {
            cfg.dismissal_threshold = v;
        }
        cfg
    }
}

/// TOML-deserialisable `[outcome]` table (all fields optional).
///
/// Why: the config file may set none, any, or all fields; optional fields
/// let absent keys fall through to the env / default layer.
/// What: an optional-field mirror of `OutcomeConfig` used only during
/// config-file parsing.
/// Test: covered by `outcome_file_configures` via `from_env_and_file`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutcomeFileConfig {
    /// `[outcome] enabled = true` opts in to outcome polling.
    pub enabled: Option<bool>,
    /// `[outcome] poll_delay_minutes = 30` overrides the default delay.
    pub poll_delay_minutes: Option<u64>,
    /// `[outcome] dismissal_threshold = 3` overrides the default threshold.
    pub dismissal_threshold: Option<u32>,
}

fn parse_bool_env(var: &str) -> Option<bool> {
    let raw = std::env::var(var).ok()?;
    let v = raw.trim().to_lowercase();
    if v.is_empty() {
        return None;
    }
    match v.as_str() {
        "false" | "0" | "no" | "off" => Some(false),
        "true" | "1" | "yes" | "on" => Some(true),
        other => {
            warn!("unrecognised boolean for {var}: {other:?} — ignoring");
            None
        }
    }
}

fn parse_u64_env(var: &str) -> Option<u64> {
    let raw = std::env::var(var).ok()?;
    match raw.trim().parse::<u64>() {
        Ok(v) => Some(v),
        Err(_) => {
            warn!("unrecognised u64 for {var}: {:?} — ignoring", raw.trim());
            None
        }
    }
}

fn parse_u32_env(var: &str) -> Option<u32> {
    let raw = std::env::var(var).ok()?;
    match raw.trim().parse::<u32>() {
        Ok(v) => Some(v),
        Err(_) => {
            warn!("unrecognised u32 for {var}: {:?} — ignoring", raw.trim());
            None
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_env() {
        unsafe {
            std::env::remove_var(ENV_OUTCOME_ENABLED);
            std::env::remove_var(ENV_OUTCOME_POLL_DELAY);
            std::env::remove_var(ENV_OUTCOME_THRESHOLD);
        }
    }

    #[test]
    fn outcome_defaults_disabled() {
        let cfg = OutcomeConfig::default();
        assert!(!cfg.enabled, "outcome polling must default OFF");
        assert_eq!(cfg.poll_delay_minutes, 60);
        assert_eq!(cfg.dismissal_threshold, 5);
    }

    #[test]
    #[serial]
    fn outcome_env_enables() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_OUTCOME_ENABLED, "true");
            std::env::set_var(ENV_OUTCOME_POLL_DELAY, "30");
            std::env::set_var(ENV_OUTCOME_THRESHOLD, "3");
        }
        let cfg = OutcomeConfig::from_env_and_file(None);
        assert!(cfg.enabled, "env true must enable outcome polling");
        assert_eq!(cfg.poll_delay_minutes, 30);
        assert_eq!(cfg.dismissal_threshold, 3);
        clear_env();
    }

    #[test]
    #[serial]
    fn outcome_file_configures() {
        clear_env();
        let file = OutcomeFileConfig {
            enabled: Some(true),
            poll_delay_minutes: Some(120),
            dismissal_threshold: Some(10),
        };
        let cfg = OutcomeConfig::from_env_and_file(Some(&file));
        assert!(cfg.enabled);
        assert_eq!(cfg.poll_delay_minutes, 120);
        assert_eq!(cfg.dismissal_threshold, 10);
        clear_env();
    }

    #[test]
    #[serial]
    fn outcome_env_beats_file() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_OUTCOME_ENABLED, "false");
        }
        let file = OutcomeFileConfig {
            enabled: Some(true),
            poll_delay_minutes: None,
            dismissal_threshold: None,
        };
        let cfg = OutcomeConfig::from_env_and_file(Some(&file));
        assert!(!cfg.enabled, "env false must override file true");
        clear_env();
    }
}
