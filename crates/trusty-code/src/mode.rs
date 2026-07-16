//! Harness mode selection: `daily-driver` (token-efficient, default) vs
//! `parity` (full-schema benchmark mode) — #2059, vision spec §5.9
//! ("Reconciliation with Parity-Spec Decision D2").
//!
//! Why: the parity spec (D2) mandates byte-identical, full tool schemas
//! across every model for benchmark fairness; the production harness wants
//! deferred schemas / compaction / per-model optimisation for real coding
//! work. §5.9's resolved decision is two modes selected by a three-tier
//! precedence, rather than picking one behaviour permanently. This module
//! is ONLY the selection mechanism — resolving which mode applies to a
//! given run and making that resolution queryable. It deliberately does
//! NOT implement any token-efficiency behaviour (deferred schemas,
//! compaction, repo-map): that is P1B's job, hooking into the branch
//! points this module's callers expose (`prompt::assemble_system_prompt_for_mode`,
//! `agent_loop::AgentLoop`'s `mode`-aware `tool_definitions`). In M1 both
//! modes are functionally identical.
//!
//! **Precedence note:** §5.9 states the three-tier hierarchy, highest to
//! lowest, as: (1) `TRUSTY_CODE_MODE` env var ("escape hatch... overrides
//! all"), (2) `task.run`'s `mode` param ("overrides setting"), (3)
//! `.claude/settings.json`'s `code_harness.mode` (the configured default).
//! [`resolve_mode`] implements EXACTLY this order. (A paraphrase of this
//! ticket's delegation put the env var last instead of first; §5.9's own
//! table and the raw issue #2059 text both independently state env-var-
//! highest, so this module follows the spec verbatim — flagged explicitly
//! for the reader rather than silently reconciled.)
//!
//! What: [`HarnessMode`] (`DailyDriver` | `Parity`, `Default` =
//! `DailyDriver`), [`HarnessMode::parse_lenient`] (case/separator-tolerant
//! string parsing), and [`resolve_mode`] (the three-tier precedence chain,
//! reading the env var and `.claude/settings.json` directly — no injected
//! clock/env abstraction, matching this crate's existing
//! `task::mock_llm::build_llm_client` precedent for a small, direct
//! `std::env`/`std::fs` read in production code).
//! Test: `mode::tests::*` (precedence order, lenient parsing, unknown-value
//! degrade-to-next-tier, settings.json presence/absence/malformed-JSON).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Escape-hatch environment variable (§5.9): `TRUSTY_CODE_MODE=parity` or
/// `daily-driver`. Highest precedence — overrides everything else.
pub const MODE_ENV_VAR: &str = "TRUSTY_CODE_MODE";

/// The two harness modes §5.9 resolves between.
///
/// Why: see module docs. A closed, two-variant enum (not a raw `String`)
/// so every downstream `match` is exhaustive and a typo can never silently
/// select an unintended mode.
/// What: `DailyDriver` (default; where P1B's token-efficiency layers will
/// apply) and `Parity` (full schemas, parity-spec benchmark mode). Wire
/// representation is `"daily-driver"` / `"parity"` (kebab-case), matching
/// §5.9's example JSON verbatim.
/// Test: `mode::tests::wire_representation_is_kebab_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessMode {
    DailyDriver,
    Parity,
}

impl Default for HarnessMode {
    /// §5.9: "`daily-driver` (DEFAULT)".
    fn default() -> Self {
        HarnessMode::DailyDriver
    }
}

impl HarnessMode {
    /// The stable wire string for this mode (matches the serde
    /// `rename_all = "kebab-case"` representation).
    ///
    /// Why: callers building a plain JSON response field (`task.run`'s
    /// immediate `{"mode": ...}`, the CLI's human-readable summary line)
    /// want this without round-tripping through `serde_json`.
    /// Test: `mode::tests::as_str_matches_serde_rename`.
    pub fn as_str(&self) -> &'static str {
        match self {
            HarnessMode::DailyDriver => "daily-driver",
            HarnessMode::Parity => "parity",
        }
    }

    /// Parse a mode string leniently: case-insensitive, `_`/`-`
    /// interchangeable, surrounding whitespace ignored.
    ///
    /// Why: every precedence tier's raw value (an env var, a JSON string
    /// field, a CLI flag) is human-typed and should not require
    /// byte-perfect casing to work.
    /// What: `None` for anything that isn't recognisably `"daily-driver"`
    /// or `"parity"` — callers treat `None` as "this source does not
    /// contribute a value", falling through to the next-lower-precedence
    /// tier (see [`resolve_mode`]'s docs for why this, rather than a hard
    /// error, was chosen).
    /// Test: `mode::tests::parse_lenient_accepts_case_and_separator_variants`,
    /// `mode::tests::parse_lenient_rejects_unknown_values`.
    pub fn parse_lenient(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "daily-driver" | "dailydriver" => Some(HarnessMode::DailyDriver),
            "parity" => Some(HarnessMode::Parity),
            _ => None,
        }
    }
}

/// Resolve the effective [`HarnessMode`] for one `task.run` call, per §5.9's
/// three-tier precedence (highest to lowest): [`MODE_ENV_VAR`] > `task_param`
/// (the `task.run` request's own `mode` field) > `.claude/settings.json`'s
/// `code_harness.mode` > [`HarnessMode::default`].
///
/// Why: the single place this precedence is implemented, so `task::protocol`
/// never has to re-derive it.
/// What: checks each tier in order; a tier whose value is absent OR fails
/// to parse (see [`HarnessMode::parse_lenient`]'s docs — this is the
/// "clear error or fall back to default" choice this ticket asks to be
/// made explicit: an invalid value at ANY tier is treated exactly like an
/// absent one, falling through to the next tier, rather than hard-erroring
/// the whole call) is skipped.
/// `project_root` is `Option` because a projectless workstream has no project
/// and therefore no `.claude/settings.json` tier to consult — that tier is
/// simply skipped, exactly as an absent file already was. Projectless is a
/// supported state, never an error, so it still resolves a mode (env var, then
/// the request's own `mode`, then the default).
/// Test: `mode::tests::env_var_wins_over_everything`,
/// `mode::tests::task_param_wins_over_settings_json`,
/// `mode::tests::settings_json_wins_over_default`,
/// `mode::tests::default_when_nothing_set`,
/// `mode::tests::invalid_env_var_falls_through_to_next_tier`,
/// `mode::tests::projectless_skips_settings_tier_and_still_resolves`.
pub fn resolve_mode(task_param: Option<&str>, project_root: Option<&Path>) -> HarnessMode {
    if let Some(mode) = std::env::var(MODE_ENV_VAR)
        .ok()
        .and_then(|v| HarnessMode::parse_lenient(&v))
    {
        return mode;
    }
    if let Some(mode) = task_param.and_then(HarnessMode::parse_lenient) {
        return mode;
    }
    if let Some(mode) = project_root.and_then(read_settings_json_mode) {
        return mode;
    }
    HarnessMode::default()
}

/// Read `<project_root>/.claude/settings.json`'s `code_harness.mode` key.
///
/// Why: the Claude-Code-compatible, project-scoped config location §5.9's
/// example JSON uses.
/// What: `None` when the file is missing, unreadable, not valid JSON, or
/// the key is absent/unparseable — absence at any step is "this source
/// does not contribute", never an error (mirrors
/// `project_context::load_project_context`'s same graceful-degrade
/// convention for a project-scoped config file).
/// Test: `mode::tests::settings_json_wins_over_default`,
/// `mode::tests::missing_settings_json_is_not_an_error`,
/// `mode::tests::malformed_settings_json_is_not_an_error`.
fn read_settings_json_mode(project_root: &Path) -> Option<HarnessMode> {
    let path = project_root.join(".claude").join("settings.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mode_str = value.get("code_harness")?.get("mode")?.as_str()?;
    HarnessMode::parse_lenient(mode_str)
}

/// Serializes every test in this crate that sets/reads the process-wide
/// [`MODE_ENV_VAR`] — `cargo test` runs tests in parallel within one binary,
/// and an unguarded `set_var`/`remove_var` pair would race across modules
/// (both here and in `task::protocol::tests`). `pub(crate)` (not private to
/// `tests` below) so other test modules needing to mutate the SAME env var
/// share ONE lock, mirroring `task::mock_llm::MOCK_LLM_ENV_LOCK`'s identical
/// cross-module rationale. A `tokio::sync::Mutex` (not `std::sync::Mutex`)
/// because `task::protocol::tests::task_run_resolves_and_reports_mode` holds
/// the guard across `.await` (spanning several `task_run` calls) — clippy's
/// `await_holding_lock` correctly flags a std mutex held that way.
#[cfg(test)]
pub(crate) static MODE_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;

    async fn with_env_mode<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = MODE_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `MODE_ENV_LOCK`.
        unsafe {
            match value {
                Some(v) => std::env::set_var(MODE_ENV_VAR, v),
                None => std::env::remove_var(MODE_ENV_VAR),
            }
        }
        let result = f();
        unsafe {
            std::env::remove_var(MODE_ENV_VAR);
        }
        result
    }

    fn project_with_settings(json: Option<&str>) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        if let Some(json) = json {
            let dir = tmp.path().join(".claude");
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join("settings.json"), json).expect("write settings.json");
        }
        tmp
    }

    #[test]
    fn wire_representation_is_kebab_case() {
        assert_eq!(
            serde_json::to_value(HarnessMode::DailyDriver).unwrap(),
            serde_json::json!("daily-driver")
        );
        assert_eq!(
            serde_json::to_value(HarnessMode::Parity).unwrap(),
            serde_json::json!("parity")
        );
    }

    #[test]
    fn as_str_matches_serde_rename() {
        for mode in [HarnessMode::DailyDriver, HarnessMode::Parity] {
            assert_eq!(
                serde_json::json!(mode.as_str()),
                serde_json::to_value(mode).unwrap()
            );
        }
    }

    #[test]
    fn parse_lenient_accepts_case_and_separator_variants() {
        for s in ["parity", "PARITY", " Parity "] {
            assert_eq!(HarnessMode::parse_lenient(s), Some(HarnessMode::Parity));
        }
        for s in [
            "daily-driver",
            "DAILY-DRIVER",
            "daily_driver",
            "dailydriver",
        ] {
            assert_eq!(
                HarnessMode::parse_lenient(s),
                Some(HarnessMode::DailyDriver)
            );
        }
    }

    #[test]
    fn parse_lenient_rejects_unknown_values() {
        assert_eq!(HarnessMode::parse_lenient("bogus"), None);
        assert_eq!(HarnessMode::parse_lenient(""), None);
    }

    #[tokio::test]
    async fn default_when_nothing_set() {
        let project = project_with_settings(None);
        with_env_mode(None, || {
            assert_eq!(
                resolve_mode(None, Some(project.path())),
                HarnessMode::DailyDriver
            );
        })
        .await;
    }

    /// A projectless run has no `.claude/settings.json` tier to read; it must
    /// still resolve a mode from the remaining tiers rather than error.
    #[tokio::test]
    async fn projectless_skips_settings_tier_and_still_resolves() {
        with_env_mode(None, || {
            assert_eq!(
                resolve_mode(None, None),
                HarnessMode::DailyDriver,
                "projectless must fall through to the default, not error"
            );
            assert_eq!(
                resolve_mode(Some("parity"), None),
                HarnessMode::Parity,
                "the request's own mode must still apply when projectless"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn settings_json_wins_over_default() {
        let project = project_with_settings(Some(r#"{"code_harness": {"mode": "parity"}}"#));
        with_env_mode(None, || {
            assert_eq!(
                resolve_mode(None, Some(project.path())),
                HarnessMode::Parity
            );
        })
        .await;
    }

    #[tokio::test]
    async fn task_param_wins_over_settings_json() {
        let project = project_with_settings(Some(r#"{"code_harness": {"mode": "parity"}}"#));
        with_env_mode(None, || {
            assert_eq!(
                resolve_mode(Some("daily-driver"), Some(project.path())),
                HarnessMode::DailyDriver
            );
        })
        .await;
    }

    #[tokio::test]
    async fn env_var_wins_over_everything() {
        let project = project_with_settings(Some(r#"{"code_harness": {"mode": "daily-driver"}}"#));
        with_env_mode(Some("parity"), || {
            assert_eq!(
                resolve_mode(Some("daily-driver"), Some(project.path())),
                HarnessMode::Parity
            );
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_env_var_falls_through_to_next_tier() {
        let project = project_with_settings(None);
        with_env_mode(Some("not-a-real-mode"), || {
            assert_eq!(
                resolve_mode(Some("parity"), Some(project.path())),
                HarnessMode::Parity
            );
        })
        .await;
    }

    #[test]
    fn missing_settings_json_is_not_an_error() {
        let project = project_with_settings(None);
        assert_eq!(read_settings_json_mode(project.path()), None);
    }

    #[test]
    fn malformed_settings_json_is_not_an_error() {
        let project = project_with_settings(Some("not valid json"));
        assert_eq!(read_settings_json_mode(project.path()), None);
    }
}
