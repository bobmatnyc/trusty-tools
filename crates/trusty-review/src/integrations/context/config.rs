//! Per-source context configuration (Phase 6, #550).
//!
//! Why: each external context source (JIRA / Confluence / GitHub Issues, and
//! later APEX) needs an independent enable flag and a retrieval mode, resolved
//! with the same env-over-TOML-over-default precedence as the rest of the
//! config module.  Critically, every source defaults to **disabled** and only
//! turns on when its credentials/base URLs are actually present — so the crate
//! works out of the box with zero Atlassian/GitHub context config, and a source
//! with no creds is simply skipped (logged once by the orchestrator) rather than
//! erroring on every review.
//!
//! What: exposes `ContextSourcesConfig` (the resolved, owned per-source
//! settings) and its TOML mirrors (`ContextSourcesFileConfig`, `SourceFileConfig`).
//! `from_env_and_file` resolves the `[context.sources.*]` tables layered under
//! env overrides.  The actual credential resolution (Atlassian creds, GitHub
//! auth) is NOT done here — that lives in the sources themselves; this struct
//! only carries the *intent* (enabled / mode) and the auto-disable signal.
//!
//! Precedence for `enabled`:
//!   1. explicit env `TRUSTY_REVIEW_CONTEXT_<SOURCE>_ENABLED` (true/false)  — wins
//!   2. explicit TOML `[context.sources.<source>] enabled = <bool>`
//!   3. default: `false` UNLESS the source's credentials are present, in which
//!      case the source auto-enables (computed by the source, not here).
//!
//! Test: `source_defaults_disabled`, `env_enables_source`,
//! `file_sets_mode`, `env_beats_file`, `mode_parses` in this module.

use serde::Deserialize;
use tracing::warn;

use super::RetrievalMode;

/// Resolved configuration for a single context source.
///
/// Why: the source constructor reads this to decide whether to run and in which
/// mode; keeping it a small owned struct makes the source trivially testable
/// (construct it directly) and free of scattered env lookups.
/// What: `enabled` is the *explicit* operator intent (`Some(true/false)`) or
/// `None` (defer to credential-presence auto-enable); `mode` is the retrieval
/// backend (default `Live`).
/// Test: `source_defaults_disabled`, `file_sets_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceConfig {
    /// Explicit enable intent.  `None` → auto-decide from credential presence
    /// (default disabled when creds absent).  `Some(true/false)` → operator
    /// override, honoured even against credential presence.
    pub enabled: Option<bool>,
    /// Retrieval backend for this source (default `Live`).
    pub mode: RetrievalMode,
}

impl SourceConfig {
    /// Resolve the *effective* enabled flag given whether credentials are present.
    ///
    /// Why: the final "should this source run?" decision combines the explicit
    /// operator intent with credential presence.  Centralising it keeps every
    /// source consistent: an explicit `true`/`false` always wins; absent an
    /// explicit flag, the source runs only when its creds are present.
    /// What: returns `self.enabled` if set; otherwise returns `creds_present`.
    /// Test: `effective_enabled_honours_explicit`, `effective_enabled_auto`.
    pub fn effective_enabled(&self, creds_present: bool) -> bool {
        self.enabled.unwrap_or(creds_present)
    }
}

/// Resolved per-source context configuration for all external sources.
///
/// Why: the runner constructs the source set from one owned struct instead of
/// re-reading env vars per source; this mirrors `ContextConfig` (#590) and keeps
/// the precedence logic in one place.
/// What: one `SourceConfig` per external source.  PR-B will add an `apex` field
/// (or a generic knowledgebase map) without disturbing these three.
/// Test: `from_env_and_file_layers_correctly`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextSourcesConfig {
    /// JIRA live source.
    pub jira: SourceConfig,
    /// Confluence live source.
    pub confluence: SourceConfig,
    /// GitHub Issues live source.
    pub github_issues: SourceConfig,
}

impl ContextSourcesConfig {
    /// Resolve from env vars layered over an optional `[context.sources]` table.
    ///
    /// Why: gives operators one mental model (env beats TOML beats default)
    /// across every source, matching the rest of `config`.
    /// What: starts from the file values (or defaults), then applies the
    /// `TRUSTY_REVIEW_CONTEXT_<SOURCE>_ENABLED` and
    /// `TRUSTY_REVIEW_CONTEXT_<SOURCE>_MODE` env overrides per source.
    /// Test: `env_enables_source`, `env_beats_file`, `file_sets_mode`.
    pub fn from_env_and_file(file: Option<&ContextSourcesFileConfig>) -> Self {
        Self {
            jira: resolve_source("JIRA", file.map(|f| &f.jira)),
            confluence: resolve_source("CONFLUENCE", file.map(|f| &f.confluence)),
            github_issues: resolve_source("GITHUB_ISSUES", file.map(|f| &f.github_issues)),
        }
    }
}

/// Resolve one source's `SourceConfig` from its file table + env overrides.
///
/// Why: the three sources resolve identically; factoring the per-source logic
/// avoids triplicated env-var plumbing.
/// What: reads `[context.sources.<source>]` (file) for the base values, then
/// overrides `enabled` from `TRUSTY_REVIEW_CONTEXT_<KEY>_ENABLED` and `mode`
/// from `TRUSTY_REVIEW_CONTEXT_<KEY>_MODE`.
/// Test: covered by `env_enables_source`, `env_beats_file`, `file_sets_mode`.
fn resolve_source(env_key: &str, file: Option<&SourceFileConfig>) -> SourceConfig {
    let mut cfg = SourceConfig {
        enabled: file.and_then(|f| f.enabled),
        mode: file.and_then(|f| f.mode).unwrap_or_default(),
    };
    if let Some(v) = parse_bool_env(&format!("TRUSTY_REVIEW_CONTEXT_{env_key}_ENABLED")) {
        cfg.enabled = Some(v);
    }
    if let Some(m) = parse_mode_env(&format!("TRUSTY_REVIEW_CONTEXT_{env_key}_MODE")) {
        cfg.mode = m;
    }
    cfg
}

// ─── TOML mirrors ───────────────────────────────────────────────────────────

/// TOML-deserialisable `[context.sources]` table.
///
/// Why: lets a config file set per-source enable/mode without env vars; nested
/// under the existing `[context]` table so all context knobs live together.
/// What: one optional `SourceFileConfig` per source; absent keys fall through to
/// the env / default layer.
/// Test: covered by `from_env_and_file` tests via injected `ContextSourcesFileConfig`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContextSourcesFileConfig {
    /// `[context.sources.jira]`.
    #[serde(default)]
    pub jira: SourceFileConfig,
    /// `[context.sources.confluence]`.
    #[serde(default)]
    pub confluence: SourceFileConfig,
    /// `[context.sources.github_issues]`.
    #[serde(default)]
    pub github_issues: SourceFileConfig,
}

/// TOML-deserialisable single-source table (all fields optional).
///
/// Why: a source's file table may set neither, either, or both knobs; optional
/// fields let an absent key fall through to env / default.
/// What: `enabled` and `mode`, both optional.
/// Test: `file_sets_mode`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourceFileConfig {
    /// `enabled = true|false`.
    pub enabled: Option<bool>,
    /// `mode = "live"|"semantic"`.
    pub mode: Option<RetrievalMode>,
}

// ─── Env parsing helpers ────────────────────────────────────────────────────

/// Parse a boolean env var with lenient truthiness, or `None` if unset/empty.
///
/// Why: env booleans come in many spellings; centralising the parse keeps every
/// source's enable flag consistent.
/// What: `Some(true)` for true/1/yes/on; `Some(false)` for false/0/no/off;
/// `None` for unset/empty; `None` (with a warning) for anything unrecognised.
/// Test: covered by `env_enables_source`.
fn parse_bool_env(var: &str) -> Option<bool> {
    let raw = std::env::var(var).ok()?;
    let v = raw.trim().to_lowercase();
    if v.is_empty() {
        return None;
    }
    match v.as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        other => {
            warn!("unrecognised boolean for {var}: {other:?} — ignoring");
            None
        }
    }
}

/// Parse a retrieval-mode env var, or `None` if unset/empty/invalid.
///
/// Why: operators can flip a source to `semantic` via env once PR-B lands; the
/// parse must be lenient and never panic.
/// What: `Some(Live)` for `live`; `Some(Semantic)` for `semantic`/`indexed`;
/// `None` otherwise (warn on unrecognised non-empty values).
/// Test: `mode_parses`.
fn parse_mode_env(var: &str) -> Option<RetrievalMode> {
    let raw = std::env::var(var).ok()?;
    let v = raw.trim().to_lowercase();
    if v.is_empty() {
        return None;
    }
    match v.as_str() {
        "live" => Some(RetrievalMode::Live),
        "semantic" | "indexed" => Some(RetrievalMode::Semantic),
        other => {
            warn!("unrecognised retrieval mode for {var}: {other:?} — ignoring");
            None
        }
    }
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_env() {
        unsafe {
            for k in [
                "TRUSTY_REVIEW_CONTEXT_JIRA_ENABLED",
                "TRUSTY_REVIEW_CONTEXT_JIRA_MODE",
                "TRUSTY_REVIEW_CONTEXT_CONFLUENCE_ENABLED",
                "TRUSTY_REVIEW_CONTEXT_CONFLUENCE_MODE",
                "TRUSTY_REVIEW_CONTEXT_GITHUB_ISSUES_ENABLED",
                "TRUSTY_REVIEW_CONTEXT_GITHUB_ISSUES_MODE",
            ] {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    fn source_defaults_disabled_without_creds() {
        let cfg = SourceConfig::default();
        // No explicit flag, no creds → disabled.
        assert!(!cfg.effective_enabled(false));
        // No explicit flag, creds present → auto-enabled.
        assert!(cfg.effective_enabled(true));
        assert_eq!(cfg.mode, RetrievalMode::Live);
    }

    #[test]
    fn effective_enabled_honours_explicit_false_even_with_creds() {
        let cfg = SourceConfig {
            enabled: Some(false),
            mode: RetrievalMode::Live,
        };
        // Explicit false wins even when creds are present.
        assert!(!cfg.effective_enabled(true));
    }

    #[test]
    fn effective_enabled_honours_explicit_true_without_creds() {
        let cfg = SourceConfig {
            enabled: Some(true),
            mode: RetrievalMode::Live,
        };
        // Explicit true is honoured; the source will still skip later if it has
        // no creds, but the *intent* is enabled.
        assert!(cfg.effective_enabled(false));
    }

    #[test]
    #[serial]
    fn from_env_and_file_defaults_disabled() {
        clear_env();
        let cfg = ContextSourcesConfig::from_env_and_file(None);
        assert_eq!(cfg.jira.enabled, None);
        assert_eq!(cfg.confluence.enabled, None);
        assert_eq!(cfg.github_issues.enabled, None);
        assert_eq!(cfg.jira.mode, RetrievalMode::Live);
        clear_env();
    }

    #[test]
    #[serial]
    fn env_enables_source() {
        clear_env();
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_CONTEXT_JIRA_ENABLED", "true");
        }
        let cfg = ContextSourcesConfig::from_env_and_file(None);
        assert_eq!(cfg.jira.enabled, Some(true));
        assert_eq!(cfg.confluence.enabled, None);
        clear_env();
    }

    #[test]
    #[serial]
    fn file_sets_mode() {
        clear_env();
        let file = ContextSourcesFileConfig {
            jira: SourceFileConfig {
                enabled: Some(true),
                mode: Some(RetrievalMode::Semantic),
            },
            ..Default::default()
        };
        let cfg = ContextSourcesConfig::from_env_and_file(Some(&file));
        assert_eq!(cfg.jira.enabled, Some(true));
        assert_eq!(cfg.jira.mode, RetrievalMode::Semantic);
        clear_env();
    }

    #[test]
    #[serial]
    fn env_beats_file() {
        clear_env();
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_CONTEXT_JIRA_ENABLED", "false");
            std::env::set_var("TRUSTY_REVIEW_CONTEXT_JIRA_MODE", "live");
        }
        let file = ContextSourcesFileConfig {
            jira: SourceFileConfig {
                enabled: Some(true),
                mode: Some(RetrievalMode::Semantic),
            },
            ..Default::default()
        };
        let cfg = ContextSourcesConfig::from_env_and_file(Some(&file));
        // Env false + live override the file's true + semantic.
        assert_eq!(cfg.jira.enabled, Some(false));
        assert_eq!(cfg.jira.mode, RetrievalMode::Live);
        clear_env();
    }

    #[test]
    #[serial]
    fn mode_parses() {
        clear_env();
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_CONTEXT_GITHUB_ISSUES_MODE", "semantic");
        }
        let cfg = ContextSourcesConfig::from_env_and_file(None);
        assert_eq!(cfg.github_issues.mode, RetrievalMode::Semantic);
        clear_env();
    }

    #[test]
    #[serial]
    fn unrecognised_env_falls_through() {
        clear_env();
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_CONTEXT_JIRA_ENABLED", "maybe");
            std::env::set_var("TRUSTY_REVIEW_CONTEXT_JIRA_MODE", "fuzzy");
        }
        let cfg = ContextSourcesConfig::from_env_and_file(None);
        assert_eq!(cfg.jira.enabled, None, "garbage enabled ignored");
        assert_eq!(cfg.jira.mode, RetrievalMode::Live, "garbage mode ignored");
        clear_env();
    }
}
