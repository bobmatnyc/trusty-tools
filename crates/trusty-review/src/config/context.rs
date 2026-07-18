//! Context-dependency requirement configuration (#590, search-unreachable
//! semantics fix).
//!
//! Why: trusty-review's entire value is the context it injects from
//! trusty-search (code context) and trusty-analyze (static analysis).  A review
//! produced WITHOUT that context is actively harmful — it gives false confidence
//! from a verdict that never saw the project.  So both dependencies are REQUIRED
//! by default; if either is unreachable the review must skip/fail loudly rather
//! than silently degrade.  This struct holds the opt-out knobs an operator can
//! flip to explicitly allow a clearly-labelled degraded run.
//!
//! `require_search` additionally supports a PER-SURFACE safe default (the
//! search-unreachable semantics fix): a caller that can post a real GitHub PR
//! review (the hosted webhook bot, a CLI GitHub-PR run) must never silently
//! degrade — search stays required unless the operator explicitly opts out. A
//! caller that can never post (an MCP tool call from a developer's own
//! session, a CLI `--local-diff`/`--base`/`--source-root` local review) is
//! still useful with a diff-only, loudly-labelled DEGRADED review, so it
//! defaults to NOT requiring search when the operator hasn't said otherwise.
//! `require_analyze` is unaffected — it keeps the single always-required-
//! unless-opted-out flag (out of scope for this fix).
//!
//! What: exposes `ContextConfig` (`require_search: Option<bool>` — `None` means
//! "no explicit operator override, resolve per `InvocationSurface`";
//! `require_analyze: bool`, defaulting to `true`) and its TOML-deserialisable
//! mirror `ContextFileConfig`.  `from_env_and_file` resolves
//! env-over-file-over-default precedence, matching the rest of the config
//! module.  `effective_require_search` folds the override (if any) and the
//! surface default into the single boolean the gate consults.
//!
//! Test: `context_defaults_required`, `context_env_relaxes_search`,
//! `context_file_relaxes_analyze`, `context_env_beats_file`,
//! `require_search_surface_default_hosted_true_interactive_false`,
//! `require_search_explicit_override_wins_regardless_of_surface` in this module.

use serde::Deserialize;
use tracing::warn;

use crate::integrations::context::ContextSourcesFileConfig;

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

/// Which kind of caller triggered a review — decides the SAFE DEFAULT for
/// `require_search` when the operator has not explicitly configured it.
///
/// Why: an infra-down review that CAN post to a real GitHub PR (the hosted
/// webhook bot) must never silently degrade — a context-free false-confidence
/// verdict landing on a real PR is exactly the harm #590 exists to prevent. A
/// review that CANNOT post (an MCP tool call the developer reads themselves, a
/// local-diff/--base/--source-root CLI run that never leaves the terminal) is
/// still useful as a loudly-labelled diff-only DEGRADED review, so hard-
/// skipping it wastes the developer's time for no safety benefit.
/// What: two variants; `Hosted` is the `#[default]` (fail-safe: an unlabelled
/// call site stays strict) so only call sites that explicitly identify
/// themselves as interactive get the relaxed default.
/// Test: `require_search_surface_default_hosted_true_interactive_false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InvocationSurface {
    /// Autonomous / postable surfaces: the GitHub webhook bot and any caller
    /// that has not explicitly opted into `Interactive`.  Defaults to
    /// `require_search = true`.
    #[default]
    Hosted,
    /// Interactive/local surfaces that can never post to a real GitHub PR: MCP
    /// tool calls (`review_pr`/`review_diff`, always `allow_posting = false`)
    /// and CLI `run --local-diff`/`--base`/`--source-root` local reviews (the
    /// `owner == "local"` sentinel forces log-only regardless of
    /// `allow_posting`).  Defaults to `require_search = false` — degrade
    /// instead of hard-skip.
    Interactive,
}

impl InvocationSurface {
    /// Return this surface's safe DEFAULT for `require_search`, used only when
    /// the operator has not set an explicit override.
    ///
    /// Why: centralises the Hosted/Interactive → true/false mapping so
    /// `ContextConfig::effective_require_search` and tests share one
    /// definition.
    /// What: `true` for `Hosted`, `false` for `Interactive`.
    /// Test: `require_search_surface_default_hosted_true_interactive_false`.
    pub fn requires_search_by_default(self) -> bool {
        matches!(self, InvocationSurface::Hosted)
    }
}

/// Resolved configuration for the required-context gate.
///
/// Why: the runner reads these flags before gathering context to decide whether
/// a missing dependency aborts the review (required) or merely tags it degraded
/// (opted out).  A single owned struct keeps the decision logic free of scattered
/// env lookups and makes the behaviour trivially testable (construct it directly).
/// What: `require_analyze` gates trusty-analyze and defaults to `true` (safe-
/// by-default: refuse to review without context).  `require_search` is
/// `Option<bool>`: `None` means "no explicit operator override" and defers to
/// `InvocationSurface::requires_search_by_default`; `Some(v)` is an explicit
/// override (from env or TOML) that always wins, regardless of surface.
/// Test: `context_defaults_required`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextConfig {
    /// `None` (default) = no explicit operator override — the EFFECTIVE
    /// requirement is resolved per-call by `effective_require_search` from the
    /// caller's `InvocationSurface`.  `Some(true)`/`Some(false)` is an explicit
    /// override that wins over the surface default either way.
    pub require_search: Option<bool>,
    /// When `true` (default), an unreachable/unhealthy trusty-analyze skips the
    /// review instead of proceeding with no static-analysis context.  Has no
    /// per-surface default (unlike `require_search`) — out of scope for the
    /// search-unreachable semantics fix.
    pub require_analyze: bool,
}

impl Default for ContextConfig {
    /// Why: the safe default is "analyze required, search resolved per-surface"
    /// so a missing dependency fails loudly rather than silently producing a
    /// context-free, false-confidence verdict (#590 binding premise) UNLESS the
    /// caller is a surface that can never post one.
    /// What: `require_search: None` (defer to surface), `require_analyze: true`.
    /// Test: `context_defaults_required`.
    fn default() -> Self {
        Self {
            require_search: None,
            require_analyze: true,
        }
    }
}

impl ContextConfig {
    /// Resolve from env vars layered over an optional `[context]` TOML table.
    ///
    /// Why: matches the rest of the config module's env-over-file-over-default
    /// precedence so operators have one mental model for every knob.
    /// What: starts from the file value (`None` if absent — no override), then
    /// applies env overrides.  Unrecognised env values are ignored with a
    /// warning (fail closed: keep the stricter file/default value rather than
    /// silently relaxing a safety gate).
    /// Test: `context_env_relaxes_search`, `context_file_relaxes_analyze`,
    /// `context_env_beats_file`.
    pub fn from_env_and_file(file: Option<&ContextFileConfig>) -> Self {
        let mut cfg = ContextConfig {
            require_search: file.and_then(|f| f.require_search),
            require_analyze: file.and_then(|f| f.require_analyze).unwrap_or(true),
        };
        if let Some(v) = parse_bool_env(ENV_REQUIRE_SEARCH) {
            cfg.require_search = Some(v);
        }
        if let Some(v) = parse_bool_env(ENV_REQUIRE_ANALYZE) {
            cfg.require_analyze = v;
        }
        cfg
    }

    /// Resolve the EFFECTIVE `require_search` flag for a given invocation surface.
    ///
    /// Why: the gate needs a single boolean; folding the explicit-override /
    /// surface-default precedence here keeps that decision in one place instead
    /// of re-derived at every call site.
    /// What: an explicit operator override (`Some(v)`, from env or TOML) always
    /// wins. When unconfigured (`None`) the surface's safe default applies.
    /// Test: `require_search_surface_default_hosted_true_interactive_false`,
    /// `require_search_explicit_override_wins_regardless_of_surface`.
    pub fn effective_require_search(&self, surface: InvocationSurface) -> bool {
        self.require_search
            .unwrap_or_else(|| surface.requires_search_by_default())
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
    /// `[context.sources.*]` — per-source enable/mode for the external context
    /// sources (JIRA / Confluence / GitHub Issues; APEX in PR-B).  Defaults to an
    /// all-default (auto-disable-without-creds) configuration when absent.
    #[serde(default)]
    pub sources: ContextSourcesFileConfig,
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
        assert_eq!(
            cfg.require_search, None,
            "search must default to NO explicit override — resolved per-surface"
        );
        assert!(cfg.require_analyze, "analyze must default to REQUIRED");
    }

    #[test]
    fn require_search_surface_default_hosted_true_interactive_false() {
        let cfg = ContextConfig::default();
        assert!(
            cfg.effective_require_search(InvocationSurface::Hosted),
            "Hosted (webhook bot / CLI GitHub-PR) must default to REQUIRED — \
             never silently degrade a review that could post to a real PR"
        );
        assert!(
            !cfg.effective_require_search(InvocationSurface::Interactive),
            "Interactive (MCP tool calls / CLI local-diff) must default to NOT \
             required — degrade instead of hard-skip"
        );
    }

    #[test]
    fn require_search_explicit_override_wins_regardless_of_surface() {
        let mut cfg = ContextConfig {
            require_search: Some(false),
            ..Default::default()
        };
        assert!(
            !cfg.effective_require_search(InvocationSurface::Hosted),
            "explicit false must override even the strict Hosted default"
        );
        cfg.require_search = Some(true);
        assert!(
            cfg.effective_require_search(InvocationSurface::Interactive),
            "explicit true must override even the relaxed Interactive default"
        );
    }

    #[test]
    #[serial]
    fn context_env_relaxes_search() {
        clear_env();
        unsafe {
            std::env::set_var(ENV_REQUIRE_SEARCH, "false");
        }
        let cfg = ContextConfig::from_env_and_file(None);
        assert_eq!(
            cfg.require_search,
            Some(false),
            "env false must set an explicit override"
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
            ..Default::default()
        };
        let cfg = ContextConfig::from_env_and_file(Some(&file));
        assert_eq!(
            cfg.require_search, None,
            "search stays unconfigured (per-surface default) absent a file value"
        );
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
            ..Default::default()
        };
        let cfg = ContextConfig::from_env_and_file(Some(&file));
        assert_eq!(
            cfg.require_search,
            Some(true),
            "env true must override file false"
        );
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
            ..Default::default()
        };
        let cfg = ContextConfig::from_env_and_file(Some(&file));
        assert!(
            !cfg.require_analyze,
            "unrecognised env must fall through to file value"
        );
        clear_env();
    }
}
