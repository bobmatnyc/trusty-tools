//! Verification-round configuration (Phase 2, #583).
//!
//! Why: the per-finding verification pass (the second LLM pass that confirms or
//! refutes findings) must be switchable off in environments where the verifier
//! model is unavailable or the extra latency/cost is unwanted, and its startup
//! liveness gate must be independently toggleable for tests and offline runs.
//! Centralising these two knobs here keeps `config/mod.rs` under the 500-line
//! cap and gives the `[verification]` TOML table a single typed home.
//!
//! What: exposes `VerificationConfig` (`enabled`, `liveness_check`) and its
//! TOML-deserialisable mirror `VerificationFileConfig`.  `from_env_and_file`
//! resolves the two-layer precedence (env var over config file over default),
//! matching the rest of the config module.
//!
//! Test: `verification_defaults_enabled`, `verification_env_disables`,
//! `verification_file_disables`, `verification_env_beats_file` in this module.

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
}

impl Default for VerificationConfig {
    /// Why: the safe default is "verification on, liveness gate on" so a
    /// mis-deployed verifier model fails loudly instead of silently
    /// auto-refuting every finding (the code-intelligence incident).
    /// What: both flags `true`.
    /// Test: `verification_defaults_enabled`.
    fn default() -> Self {
        Self {
            enabled: true,
            liveness_check: true,
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
    /// `verification_env_beats_file`.
    pub fn from_env_and_file(file: Option<&VerificationFileConfig>) -> Self {
        let mut cfg = VerificationConfig {
            enabled: file.and_then(|f| f.enabled).unwrap_or(true),
            liveness_check: file.and_then(|f| f.liveness_check).unwrap_or(true),
        };
        if let Some(v) = parse_bool_env(ENV_VERIFICATION_ENABLED) {
            cfg.enabled = v;
        }
        if let Some(v) = parse_bool_env(ENV_LIVENESS_CHECK) {
            cfg.liveness_check = v;
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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_env() {
        unsafe {
            std::env::remove_var(ENV_VERIFICATION_ENABLED);
            std::env::remove_var(ENV_LIVENESS_CHECK);
        }
    }

    #[test]
    fn verification_defaults_enabled() {
        let cfg = VerificationConfig::default();
        assert!(cfg.enabled, "verification must default ON");
        assert!(cfg.liveness_check, "liveness gate must default ON");
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
        };
        let cfg = VerificationConfig::from_env_and_file(Some(&file));
        assert!(
            !cfg.enabled,
            "unrecognised env must fall through to file value"
        );
        clear_env();
    }
}
