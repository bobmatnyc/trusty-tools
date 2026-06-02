//! Context-dependency requirement configuration (#590).
//!
//! Why: trusty-review's entire value is the context it injects from
//! trusty-search (code context) and trusty-analyze (static analysis).  A review
//! produced WITHOUT that context is actively harmful — it gives false confidence
//! from a verdict that never saw the project.  So both dependencies are REQUIRED
//! by default; if either is unreachable the review must skip/fail loudly rather
//! than silently degrade.  This struct holds the two opt-out knobs an operator
//! can flip (to `false`) to explicitly allow a clearly-labelled degraded run.
//!
//! What: exposes `ContextConfig` (`require_search`, `require_analyze`, both
//! defaulting to `true`) and its TOML-deserialisable mirror
//! `ContextFileConfig`.  `from_env_and_file` resolves env-over-file-over-default
//! precedence, matching the rest of the config module.
//!
//! Test: `context_defaults_required`, `context_env_relaxes_search`,
//! `context_file_relaxes_analyze`, `context_env_beats_file` in this module.

use serde::Deserialize;
use tracing::warn;

/// Environment variable that toggles whether trusty-search is a hard requirement.
///
/// Why: operators need a discoverable single switch to opt into a degraded run
/// (e.g. an air-gapped CI box with no search daemon) without editing TOML.
/// What: any of `false`/`0`/`no`/`off` (case-insensitive) relaxes the
/// requirement; anything else (or unset) leaves the file / default value (true).
const ENV_REQUIRE_SEARCH: &str = "TRUSTY_REVIEW_REQUIRE_SEARCH";

/// Environment variable that toggles whether trusty-analyze is a hard requirement.
///
/// Why: same opt-out as `ENV_REQUIRE_SEARCH`, scoped to the analyze sidecar.
/// What: same truthiness parsing as `ENV_REQUIRE_SEARCH`.
const ENV_REQUIRE_ANALYZE: &str = "TRUSTY_REVIEW_REQUIRE_ANALYZE";

/// Resolved configuration for the required-context gate.
///
/// Why: the runner reads these flags before gathering context to decide whether
/// a missing dependency aborts the review (required) or merely tags it degraded
/// (opted out).  A single owned struct keeps the decision logic free of scattered
/// env lookups and makes the behaviour trivially testable (construct it directly).
/// What: `require_search` / `require_analyze` each gate one dependency.  Both
/// default to `true` (safe-by-default: refuse to review without context).
/// Test: `context_defaults_required`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextConfig {
    /// When `true` (default), an unreachable/unhealthy trusty-search skips the
    /// review instead of proceeding with no code context.
    pub require_search: bool,
    /// When `true` (default), an unreachable/unhealthy trusty-analyze skips the
    /// review instead of proceeding with no static-analysis context.
    pub require_analyze: bool,
}

impl Default for ContextConfig {
    /// Why: the safe default is "both required" so a missing dependency fails
    /// loudly rather than silently producing a context-free, false-confidence
    /// verdict (#590 binding premise).
    /// What: both flags `true`.
    /// Test: `context_defaults_required`.
    fn default() -> Self {
        Self {
            require_search: true,
            require_analyze: true,
        }
    }
}

impl ContextConfig {
    /// Resolve from env vars layered over an optional `[context]` TOML table.
    ///
    /// Why: matches the rest of the config module's env-over-file-over-default
    /// precedence so operators have one mental model for every knob.
    /// What: starts from the file value (or default `true`), then applies env
    /// overrides.  Unrecognised env values are ignored with a warning (fail
    /// closed: keep the stricter file/default value rather than silently
    /// relaxing a safety gate).
    /// Test: `context_env_relaxes_search`, `context_file_relaxes_analyze`,
    /// `context_env_beats_file`.
    pub fn from_env_and_file(file: Option<&ContextFileConfig>) -> Self {
        let mut cfg = ContextConfig {
            require_search: file.and_then(|f| f.require_search).unwrap_or(true),
            require_analyze: file.and_then(|f| f.require_analyze).unwrap_or(true),
        };
        if let Some(v) = parse_bool_env(ENV_REQUIRE_SEARCH) {
            cfg.require_search = v;
        }
        if let Some(v) = parse_bool_env(ENV_REQUIRE_ANALYZE) {
            cfg.require_analyze = v;
        }
        cfg
    }
}

/// TOML-deserialisable `[context]` table (all fields optional).
///
/// Why: the config file may set neither, either, or both flags; optional fields
/// let an absent key fall through to the env / default layer.
/// What: an optional-field mirror of `ContextConfig` used only during config-file
/// parsing.
/// Test: covered by `context_file_relaxes_analyze` via `from_env_and_file`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContextFileConfig {
    /// `[context] require_search = false` opts into a degraded run when search
    /// is unavailable.
    pub require_search: Option<bool>,
    /// `[context] require_analyze = false` opts into a degraded run when analyze
    /// is unavailable.
    pub require_analyze: Option<bool>,
}

/// Parse a boolean env var with lenient truthiness, or `None` if unset/empty.
///
/// Why: env-var booleans come in many spellings; centralising the parse keeps
/// the two flags consistent and avoids silently treating `"false"` as truthy.
/// What: returns `Some(false)` for `false`/`0`/`no`/`off`, `Some(true)` for
/// `true`/`1`/`yes`/`on`, `None` for unset/empty, and `None` (with a warning)
/// for anything unrecognised.
/// Test: covered indirectly by `context_env_relaxes_search`.
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
            std::env::remove_var(ENV_REQUIRE_SEARCH);
            std::env::remove_var(ENV_REQUIRE_ANALYZE);
        }
    }

    #[test]
    fn context_defaults_required() {
        let cfg = ContextConfig::default();
        assert!(cfg.require_search, "search must default to REQUIRED");
        assert!(cfg.require_analyze, "analyze must default to REQUIRED");
    }

    #[test]
    #[serial]
    fn context_env_relaxes_search() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_REQUIRE_SEARCH, "false");
        }
        let cfg = ContextConfig::from_env_and_file(None);
        assert!(
            !cfg.require_search,
            "env false must relax search requirement"
        );
        assert!(cfg.require_analyze, "analyze untouched by search var");
        clear_env();
    }

    #[test]
    #[serial]
    fn context_file_relaxes_analyze() {
        clear_env();
        let file = ContextFileConfig {
            require_search: None,
            require_analyze: Some(false),
        };
        let cfg = ContextConfig::from_env_and_file(Some(&file));
        assert!(cfg.require_search, "search stays required by default");
        assert!(
            !cfg.require_analyze,
            "file false must relax analyze requirement"
        );
        clear_env();
    }

    #[test]
    #[serial]
    fn context_env_beats_file() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_REQUIRE_SEARCH, "true");
        }
        // File says relaxed, env says required → env wins (fail closed).
        let file = ContextFileConfig {
            require_search: Some(false),
            require_analyze: None,
        };
        let cfg = ContextConfig::from_env_and_file(Some(&file));
        assert!(cfg.require_search, "env true must override file false");
        clear_env();
    }

    #[test]
    #[serial]
    fn context_unrecognised_env_keeps_file_value() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_REQUIRE_ANALYZE, "maybe");
        }
        let file = ContextFileConfig {
            require_search: None,
            require_analyze: Some(false),
        };
        let cfg = ContextConfig::from_env_and_file(Some(&file));
        assert!(
            !cfg.require_analyze,
            "unrecognised env must fall through to file value"
        );
        clear_env();
    }
}
