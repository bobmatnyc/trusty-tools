//! Verification-round configuration (Phase 2, #583).
//!
//! Why: the per-finding verification pass (the second LLM pass that confirms or
//! refutes findings) must be switchable off in environments where the verifier
//! model is unavailable or the extra latency/cost is unwanted, and its startup
//! liveness gate must be independently toggleable for tests and offline runs.
//! Centralising these two knobs here keeps `config/mod.rs` under the 500-line
//! cap and gives the `[verification]` TOML table a single typed home.
//!
//! What: exposes `VerificationConfig` (`enabled`, `liveness_check`,
//! `concurrency`, `max_attempts`) and its TOML-deserialisable mirror
//! `VerificationFileConfig`.  `from_env_and_file` resolves the two-layer
//! precedence (env var over config file over default), matching the rest of the
//! config module.
//!
//! `concurrency` and `max_attempts` arrived with #4459: the verifier fan-out
//! ceiling used to be a module-private constant in `pipeline::verify`, so an
//! operator whose provider was throttling the round had no lever short of a
//! release.  `pipeline::verify::VerifyPolicy` reads both.
//!
//! Test: `verification_defaults_enabled`, `verification_env_disables`,
//! `verification_file_disables`, `verification_env_beats_file`,
//! `verify_fan_out_knobs_resolve_and_clamp` in this module.

use serde::Deserialize;
use tracing::warn;

/// Environment variable that toggles the whole verification round.
///
/// Why: operators need a single, discoverable switch to disable verification
/// without editing a TOML file (e.g. when the verifier model is being migrated).
/// What: any of `false`/`0`/`no`/`off` (case-insensitive) disables it; anything
/// else (or unset) leaves the config-file / default value in force.
const ENV_VERIFICATION_ENABLED: &str = "TRUSTY_REVIEW_VERIFICATION_ENABLED";

/// Environment variable that toggles only the startup verifier-model liveness gate.
///
/// Why: the liveness probe makes a real (cheap) network call to the verifier
/// model; offline/CI runs need to disable just that probe while keeping the
/// verification logic itself testable with injected fakes.
/// What: same truthiness parsing as `ENV_VERIFICATION_ENABLED`.
const ENV_LIVENESS_CHECK: &str = "TRUSTY_REVIEW_VERIFIER_LIVENESS_CHECK";

/// Environment variable that sets how many verifier calls run at once (#4459).
///
/// Why: the fan-out ceiling was a module-private constant in `pipeline::verify`,
/// so the only way to relieve a transport-error storm was a code change and a
/// release. An operator hitting provider throttling needs to turn it down now.
/// What: a positive integer; `0` and unparseable values are ignored with a
/// warning, keeping the file/default value.
const ENV_VERIFY_CONCURRENCY: &str = "TRUSTY_REVIEW_VERIFY_CONCURRENCY";

/// Environment variable that sets the per-finding verifier attempt budget (#4459).
///
/// Why: the retry budget trades wall-clock latency against how many findings
/// come back UNVERIFIED under provider pressure, and the right trade differs
/// per deployment.
/// What: a positive integer counting TOTAL attempts (1 = no retry); `0` and
/// unparseable values are ignored with a warning.
const ENV_VERIFY_MAX_ATTEMPTS: &str = "TRUSTY_REVIEW_VERIFY_MAX_ATTEMPTS";

/// Verifier calls in flight per verification round when nothing overrides it.
///
/// Why: unchanged from the pre-#4459 hardcoded value, so an operator who sets
/// nothing sees the same fan-out width as before; what changed is that the
/// retry ladder now absorbs the transport errors that width produces.
/// What: the default for [`VerificationConfig::concurrency`].
pub const DEFAULT_VERIFY_CONCURRENCY: usize = 4;

/// Total verifier attempts per finding when nothing overrides it (#4459).
///
/// Why: three attempts across the default backoff ladder covers the transport
/// blips and 429s a concurrent fan-out produces without stalling a review when
/// the provider is genuinely down.
/// What: the default for [`VerificationConfig::max_attempts`]; counts the first
/// call, so `1` disables retry.
pub const DEFAULT_VERIFY_MAX_ATTEMPTS: u32 = 3;

/// Resolved configuration for the verification round.
///
/// Why: the runner and the `serve` startup path both read these flags; a single
/// owned struct keeps the decision logic free of scattered env lookups and makes
/// the behaviour trivially testable (construct the struct directly).
/// What: `enabled` gates whether the verification pass runs at all; `liveness_check`
/// gates whether `serve`/`run --live` refuse to start when the verifier model is
/// unavailable.  Both default to `true` (safe-by-default: verify, and refuse to
/// run live against a dead verifier).
/// Test: `verification_defaults_enabled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationConfig {
    /// When `true` (default), the per-finding verification round runs between
    /// verdict parse and finalisation.
    pub enabled: bool,
    /// When `true` (default), live mode refuses to start if the startup
    /// verifier-model liveness probe fails with a config/lifecycle error.
    pub liveness_check: bool,
    /// Verifier calls in flight per round (#4459). Always ≥ 1.
    pub concurrency: usize,
    /// Total verifier attempts per finding, first call included (#4459).
    /// Always ≥ 1; `1` disables retry.
    pub max_attempts: u32,
}

impl Default for VerificationConfig {
    /// Why: the safe default is "verification on, liveness gate on" so a
    /// mis-deployed verifier model fails loudly instead of silently
    /// auto-refuting every finding (the code-intelligence incident).
    /// What: both flags `true`; the fan-out knobs take
    /// [`DEFAULT_VERIFY_CONCURRENCY`] / [`DEFAULT_VERIFY_MAX_ATTEMPTS`].
    /// Test: `verification_defaults_enabled`.
    fn default() -> Self {
        Self {
            enabled: true,
            liveness_check: true,
            concurrency: DEFAULT_VERIFY_CONCURRENCY,
            max_attempts: DEFAULT_VERIFY_MAX_ATTEMPTS,
        }
    }
}

impl VerificationConfig {
    /// Resolve from env vars layered over an optional `[verification]` TOML table.
    ///
    /// Why: matches the rest of the config module's env-over-file-over-default
    /// precedence so operators have one mental model for every knob.
    /// What: starts from the file value (or default), then applies env overrides.
    /// Unrecognised env values are ignored with a warning (fail-open: keep the
    /// stricter file/default value rather than silently flipping a safety gate).
    /// Test: `verification_env_disables`, `verification_file_disables`,
    /// `verification_env_beats_file`, `verify_fan_out_knobs_resolve_and_clamp`.
    pub fn from_env_and_file(file: Option<&VerificationFileConfig>) -> Self {
        let mut cfg = VerificationConfig {
            enabled: file.and_then(|f| f.enabled).unwrap_or(true),
            liveness_check: file.and_then(|f| f.liveness_check).unwrap_or(true),
            concurrency: file
                .and_then(|f| f.concurrency)
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_VERIFY_CONCURRENCY),
            max_attempts: file
                .and_then(|f| f.max_attempts)
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_VERIFY_MAX_ATTEMPTS),
        };
        if let Some(v) = parse_bool_env(ENV_VERIFICATION_ENABLED) {
            cfg.enabled = v;
        }
        if let Some(v) = parse_bool_env(ENV_LIVENESS_CHECK) {
            cfg.liveness_check = v;
        }
        if let Some(v) = parse_positive_env(ENV_VERIFY_CONCURRENCY) {
            cfg.concurrency = v as usize;
        }
        if let Some(v) = parse_positive_env(ENV_VERIFY_MAX_ATTEMPTS) {
            cfg.max_attempts = v;
        }
        cfg
    }
}

/// TOML-deserialisable `[verification]` table (all fields optional).
///
/// Why: the config file may set neither, either, or both flags; optional fields
/// let an absent key fall through to the env / default layer.
/// What: an optional-field mirror of `VerificationConfig` used only during
/// config-file parsing.
/// Test: covered by `verification_file_disables` via `from_env_and_file`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VerificationFileConfig {
    /// `[verification] enabled = false` disables the whole round.
    pub enabled: Option<bool>,
    /// `[verification] liveness_check = false` disables only the startup gate.
    pub liveness_check: Option<bool>,
    /// `[verification] concurrency = 2` narrows the verifier fan-out (#4459).
    pub concurrency: Option<usize>,
    /// `[verification] max_attempts = 5` widens the per-finding retry budget (#4459).
    pub max_attempts: Option<u32>,
}

/// Parse a boolean env var with lenient truthiness, or `None` if unset/empty.
///
/// Why: env-var booleans come in many spellings; centralising the parse keeps
/// the two flags consistent and avoids silently treating `"false"` as truthy.
/// What: returns `Some(false)` for `false`/`0`/`no`/`off`, `Some(true)` for
/// `true`/`1`/`yes`/`on`, `None` for unset/empty, and `None` (with a warning)
/// for anything unrecognised.
/// Test: covered indirectly by `verification_env_disables` /
/// `verification_env_beats_file`.
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

/// Parse a strictly positive integer env var, or `None` if unset/invalid.
///
/// Why: both fan-out knobs are counts where `0` means "verify nothing" — a
/// typo that would silently disable the anti-fabrication net is the exact
/// failure #4459 is about, so `0` is rejected rather than honoured.
/// What: returns `Some(n)` for `n >= 1`; `None` (with a warning) for `0`,
/// negatives, non-numbers, and unset/empty.
/// Test: `verify_fan_out_knobs_resolve_and_clamp`.
fn parse_positive_env(var: &str) -> Option<u32> {
    let raw = std::env::var(var).ok()?;
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    match v.parse::<u32>() {
        Ok(n) if n >= 1 => Some(n),
        _ => {
            warn!("{var} must be a positive integer, got {v:?} — ignoring");
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
            std::env::remove_var(ENV_VERIFICATION_ENABLED);
            std::env::remove_var(ENV_LIVENESS_CHECK);
            std::env::remove_var(ENV_VERIFY_CONCURRENCY);
            std::env::remove_var(ENV_VERIFY_MAX_ATTEMPTS);
        }
    }

    #[test]
    fn verification_defaults_enabled() {
        let cfg = VerificationConfig::default();
        assert!(cfg.enabled, "verification must default ON");
        assert!(cfg.liveness_check, "liveness gate must default ON");
        assert_eq!(cfg.concurrency, DEFAULT_VERIFY_CONCURRENCY);
        assert_eq!(cfg.max_attempts, DEFAULT_VERIFY_MAX_ATTEMPTS);
    }

    /// #4459: the fan-out ceiling and the retry budget are operator knobs, and a
    /// `0` on either must not disable verification by the back door.
    #[test]
    #[serial]
    fn verify_fan_out_knobs_resolve_and_clamp() {
        clear_env();
        let file = VerificationFileConfig {
            concurrency: Some(0),
            max_attempts: Some(7),
            ..Default::default()
        };
        let cfg = VerificationConfig::from_env_and_file(Some(&file));
        assert_eq!(
            cfg.concurrency, DEFAULT_VERIFY_CONCURRENCY,
            "a 0 in the file must fall through to the default, never to no verification"
        );
        assert_eq!(
            cfg.max_attempts, 7,
            "a positive file value must be honoured"
        );

        unsafe {
            std::env::set_var(ENV_VERIFY_CONCURRENCY, "2");
            std::env::set_var(ENV_VERIFY_MAX_ATTEMPTS, "0");
        }
        let cfg = VerificationConfig::from_env_and_file(Some(&file));
        assert_eq!(cfg.concurrency, 2, "env must override the file value");
        assert_eq!(
            cfg.max_attempts, 7,
            "a 0 in the env must be ignored, leaving the file value in force"
        );
        clear_env();
    }

    #[test]
    #[serial]
    fn verification_env_disables() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_VERIFICATION_ENABLED, "false");
        }
        let cfg = VerificationConfig::from_env_and_file(None);
        assert!(!cfg.enabled, "env false must disable verification");
        assert!(cfg.liveness_check, "liveness untouched by enabled var");
        clear_env();
    }

    #[test]
    #[serial]
    fn verification_file_disables() {
        clear_env();
        let file = VerificationFileConfig {
            enabled: Some(false),
            liveness_check: Some(false),
            ..Default::default()
        };
        let cfg = VerificationConfig::from_env_and_file(Some(&file));
        assert!(!cfg.enabled, "file false must disable verification");
        assert!(!cfg.liveness_check, "file false must disable liveness gate");
        clear_env();
    }

    #[test]
    #[serial]
    fn verification_env_beats_file() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_VERIFICATION_ENABLED, "true");
        }
        // File says disabled, env says enabled → env wins.
        let file = VerificationFileConfig {
            enabled: Some(false),
            liveness_check: None,
            ..Default::default()
        };
        let cfg = VerificationConfig::from_env_and_file(Some(&file));
        assert!(cfg.enabled, "env true must override file false");
        clear_env();
    }

    #[test]
    #[serial]
    fn verification_unrecognised_env_keeps_file_value() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_VERIFICATION_ENABLED, "maybe");
        }
        let file = VerificationFileConfig {
            enabled: Some(false),
            liveness_check: None,
            ..Default::default()
        };
        let cfg = VerificationConfig::from_env_and_file(Some(&file));
        assert!(
            !cfg.enabled,
            "unrecognised env must fall through to file value"
        );
        clear_env();
    }
}
