//! `[review]` config-file section and named review-template resolution
//! (issue #2995).
//!
//! Why: extracted from `config/mod.rs` to keep that file under the 500-line
//! cap, mirroring the `voice` submodule's pattern.  A review template names a
//! markdown addendum (resolved by `crate::review_template::ReviewTemplateLoader`)
//! appended to the layered system prompt, letting a project select a review
//! rubric extension without an env var on every contributor's machine.
//! What: `ReviewFileConfig` (the TOML `[review]` table) and
//! `load_review_template`, the four-tier precedence resolver: CLI flag → repo
//! `.trusty-review.toml` → env var → the caller-selected config file (either an
//! explicit `--config` file or the global XDG default, whichever `ReviewConfig`
//! was asked to load).
//! Test: `review_template_from_cli`, `review_template_from_repo_file_beats_env`,
//! `review_template_from_env`, `review_template_from_config_file`,
//! `review_template_precedence_cli_wins_all`, `review_template_none_by_default`.

use serde::Deserialize;

/// `[review]` section of the TOML config file.
///
/// Why: names the review template (PR-review pipeline addendum) a project or
/// operator wants applied; distinct from `[report]` template selection, which
/// serves the unrelated due-diligence report pipeline.
/// What: `template` names the template to load (e.g. `"strict-security"`).
/// `None`/absent = no review template (opt-in, stock rubric unchanged).
/// Test: covered indirectly by `ReviewConfig::from_env_and_file`.
#[derive(Debug, Default, Deserialize)]
pub struct ReviewFileConfig {
    /// Name of the review template to load and append as a prompt addendum.
    #[serde(default)]
    pub template: Option<String>,
}

/// Resolve the review-template name from CLI flag, repo file, env var, or the
/// caller-selected config file, in that precedence order.
///
/// Why: `--review-template` on `run`/`compare` is the most specific override
/// (issue #2995); an auto-discovered project `.trusty-review.toml` should not
/// be silently overridden by a developer's ambient `TRUSTY_REVIEW_TEMPLATE`
/// env var, but a genuinely global config file (with no project file present)
/// keeps the pre-existing "env wins over file" relationship intact — see the
/// module doc for the full precedence discussion in `config/mod.rs`.
/// What: returns the first non-empty (trimmed) value found across, in order:
/// `cli_override`, `repo_file.template`, `TRUSTY_REVIEW_TEMPLATE`,
/// `file.template`. `None` when none of the four sources specify a name.
/// Test: `review_template_from_cli`, `review_template_from_repo_file_beats_env`,
/// `review_template_from_env`, `review_template_from_config_file`,
/// `review_template_precedence_cli_wins_all`, `review_template_none_by_default`.
pub fn load_review_template(
    cli_override: Option<&str>,
    repo_file: Option<&ReviewFileConfig>,
    file: Option<&ReviewFileConfig>,
) -> Option<String> {
    if let Some(name) = cli_override
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_string());
    }
    if let Some(name) = repo_file
        .and_then(|f| f.template.as_ref())
        .filter(|s| !s.trim().is_empty())
    {
        return Some(name.trim().to_string());
    }
    if let Ok(val) = std::env::var("TRUSTY_REVIEW_TEMPLATE") {
        let trimmed = val.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    file.and_then(|f| f.template.as_ref())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // `#[serial_test::serial]` here uses the SAME unnamed/default lock as
    // `voice.rs`'s and `pipeline::voice_config`'s env-mutating tests — all
    // three modules touch overlapping `TRUSTY_REVIEW_*` env vars, so they
    // must share one lock (a per-module named lock would NOT prevent a race
    // against the other modules' tests).

    fn file_with(name: &str) -> ReviewFileConfig {
        ReviewFileConfig {
            template: Some(name.to_string()),
        }
    }

    #[test]
    fn review_template_none_by_default() {
        assert_eq!(load_review_template(None, None, None), None);
    }

    #[test]
    #[serial_test::serial]
    fn review_template_from_cli() {
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_TEMPLATE");
        }
        let result = load_review_template(Some("strict-security"), None, None);
        assert_eq!(result.as_deref(), Some("strict-security"));
    }

    #[test]
    #[serial_test::serial]
    fn review_template_from_repo_file_beats_env() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_TEMPLATE", "env-template");
        }
        let repo = file_with("repo-template");
        let result = load_review_template(None, Some(&repo), None);
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_TEMPLATE");
        }
        assert_eq!(
            result.as_deref(),
            Some("repo-template"),
            "repo .trusty-review.toml must win over the ambient env var"
        );
    }

    #[test]
    #[serial_test::serial]
    fn review_template_from_env() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_TEMPLATE", "env-template");
        }
        let result = load_review_template(None, None, None);
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_TEMPLATE");
        }
        assert_eq!(result.as_deref(), Some("env-template"));
    }

    #[test]
    #[serial_test::serial]
    fn review_template_from_config_file() {
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_TEMPLATE");
        }
        let file = file_with("file-template");
        let result = load_review_template(None, None, Some(&file));
        assert_eq!(result.as_deref(), Some("file-template"));
    }

    #[test]
    #[serial_test::serial]
    fn review_template_precedence_cli_wins_all() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_TEMPLATE", "env-template");
        }
        let repo = file_with("repo-template");
        let file = file_with("file-template");
        let result = load_review_template(Some("cli-template"), Some(&repo), Some(&file));
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_TEMPLATE");
        }
        assert_eq!(result.as_deref(), Some("cli-template"));
    }

    #[test]
    fn review_template_empty_cli_override_falls_through() {
        let file = file_with("file-template");
        let result = load_review_template(Some("   "), None, Some(&file));
        assert_eq!(
            result.as_deref(),
            Some("file-template"),
            "a whitespace-only CLI override must not shadow the file value"
        );
    }
}
