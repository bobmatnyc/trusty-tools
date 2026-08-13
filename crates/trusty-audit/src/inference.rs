//! Which LLM the audit's review inference actually runs on (#5671).
//!
//! Why: the engagement mints a spend-capped OpenRouter key and #5663 puts it in
//! the `tga audit` child's environment. That was not enough to reach OpenRouter.
//! `trusty-review` defaults to Bedrock — `RoleModels::resolve` hardcodes
//! `Provider::Bedrock` as the last precedence level for all three roles — so the
//! key sat unread while the reviewer either failed on missing AWS credentials or
//! silently billed Bedrock. Naming the key is not selecting the provider; this
//! module selects it.
//!
//! What: [`inference_env`] returns the `TRUSTY_REVIEW_*` pairs the child needs,
//! given the engagement config and a lookup into the operator's environment.
//! The four variables are `trusty-review`'s own second precedence layer
//! (`crates/trusty-review/src/config/role_models.rs`, `RoleEnv::from_env`), so
//! this reaches the reviewer through a documented interface rather than a new
//! one. The scope is deliberately narrow: OpenRouter is selected for THIS child
//! process only. `trusty-review`'s own default stays Bedrock.
//!
//! Precedence, highest first — an operator is never silently overridden:
//!
//! 1. The variable already set in the process environment. The child inherits
//!    it, so this module emits nothing for that variable at all.
//! 2. The engagement config's `[models]` table ([`ModelPins`]).
//! 3. The built-in slugs below.
//!
//! Test: `super::inference_tests`, and
//! `crate::run::run_tests::the_child_environment_selects_openrouter_and_all_three_models`.

use crate::config::{EngagementConfig, ModelPins};

/// The variable `trusty-review` reads its provider from.
pub const ENV_PROVIDER: &str = "TRUSTY_REVIEW_PROVIDER";

/// The variable `trusty-review` reads the reviewer role's model from.
pub const ENV_REVIEWER_MODEL: &str = "TRUSTY_REVIEW_REVIEWER_MODEL";

/// The variable `trusty-review` reads the verifier role's model from.
pub const ENV_VERIFIER_MODEL: &str = "TRUSTY_REVIEW_VERIFIER_MODEL";

/// The variable `trusty-review` reads the summarizer role's model from.
pub const ENV_SUMMARIZER_MODEL: &str = "TRUSTY_REVIEW_SUMMARIZER_MODEL";

/// The provider name `trusty-review`'s `Provider: FromStr` accepts for OpenRouter.
pub const PROVIDER_OPENROUTER: &str = "openrouter";

/// Reviewer model — the highest-quality call in the pipeline, so Sonnet-class.
///
/// Why: matches the role/cost split `trusty-review` already uses for Bedrock
/// (Sonnet reviewer, Haiku verifier and summarizer), so switching provider does
/// not silently change the quality tier as well.
/// What: OpenRouter names Anthropic models `anthropic/claude-<tier>-<major>.<minor>`
/// — a DOT, not a dash. Verified against `GET https://openrouter.ai/api/v1/models`
/// on 2026-08-13, which lists `anthropic/claude-sonnet-4.6` and no dashed
/// `-4-6` form. The dashed spelling in `trusty-agents` is that crate's own
/// convention and is not an OpenRouter slug; sending it produces the HTTP 400
/// `is not a valid model ID` this constant exists to avoid.
pub const DEFAULT_REVIEWER_MODEL: &str = "anthropic/claude-sonnet-4.6";

/// Verifier model — short, high-volume calls, so the cheapest Haiku tier.
///
/// A live audit run completed on this exact slug, which is the strongest
/// evidence available for it; it is also present in OpenRouter's model list.
pub const DEFAULT_VERIFIER_MODEL: &str = "anthropic/claude-haiku-4.5";

/// Summarizer model — deterministic, low-stakes, same tier as the verifier.
pub const DEFAULT_SUMMARIZER_MODEL: &str = "anthropic/claude-haiku-4.5";

/// Build the `TRUSTY_REVIEW_*` pairs to set on the `tga audit` child.
///
/// # Postconditions
///
/// With a non-blank engagement key and an empty operator environment, the
/// result names all four variables: [`ENV_PROVIDER`] set to
/// [`PROVIDER_OPENROUTER`], and one model id per role. Every variable the
/// `operator` lookup already answers is ABSENT from the result, so the child
/// inherits the operator's value rather than this crate's. With a blank key the
/// result is empty — an engagement that configured no credential must not be
/// pointed at a provider it cannot authenticate against.
///
/// `operator` is a parameter rather than a direct `std::env::var` call for the
/// same reason [`crate::workdir::WorkDir::resolve`] takes the environment: the
/// precedence rule is then provable without mutating the process environment,
/// which is racy across a parallel test binary.
/// Test: `super::inference_tests::an_operator_variable_is_never_overridden`.
pub fn inference_env<F>(config: &EngagementConfig, operator: F) -> Vec<(&'static str, String)>
where
    F: Fn(&str) -> Option<String>,
{
    if config.openrouter_key.is_empty() {
        return Vec::new();
    }
    let pins: &ModelPins = &config.models;
    [
        (ENV_PROVIDER, pins.provider.as_deref(), PROVIDER_OPENROUTER),
        (
            ENV_REVIEWER_MODEL,
            pins.reviewer.as_deref(),
            DEFAULT_REVIEWER_MODEL,
        ),
        (
            ENV_VERIFIER_MODEL,
            pins.verifier.as_deref(),
            DEFAULT_VERIFIER_MODEL,
        ),
        (
            ENV_SUMMARIZER_MODEL,
            pins.summarizer.as_deref(),
            DEFAULT_SUMMARIZER_MODEL,
        ),
    ]
    .into_iter()
    .filter(|(name, _, _)| operator(name).is_none())
    .map(|(name, pinned, fallback)| (name, pinned.unwrap_or(fallback).to_owned()))
    .collect()
}

#[cfg(test)]
mod inference_tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    const CONFIG: &str = r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    fn config_from(text: &str) -> EngagementConfig {
        EngagementConfig::from_toml(text, Path::new("engagement.toml")).expect("parses")
    }

    fn nothing_set(_: &str) -> Option<String> {
        None
    }

    fn pairs(config: &EngagementConfig) -> HashMap<&'static str, String> {
        inference_env(config, nothing_set).into_iter().collect()
    }

    #[test]
    fn the_defaults_select_openrouter_and_all_three_roles() {
        let env = pairs(&config_from(CONFIG));
        assert_eq!(
            env.get(ENV_PROVIDER).map(String::as_str),
            Some("openrouter")
        );
        assert_eq!(
            env.get(ENV_REVIEWER_MODEL).map(String::as_str),
            Some(DEFAULT_REVIEWER_MODEL)
        );
        assert_eq!(
            env.get(ENV_VERIFIER_MODEL).map(String::as_str),
            Some(DEFAULT_VERIFIER_MODEL)
        );
        assert_eq!(
            env.get(ENV_SUMMARIZER_MODEL).map(String::as_str),
            Some(DEFAULT_SUMMARIZER_MODEL)
        );
    }

    /// The slugs are OpenRouter's, and OpenRouter spells the version with a dot.
    /// A dashed slug is the exact input that produced the HTTP 400 in #5671.
    #[test]
    fn the_default_slugs_use_openrouters_dotted_spelling() {
        for slug in [
            DEFAULT_REVIEWER_MODEL,
            DEFAULT_VERIFIER_MODEL,
            DEFAULT_SUMMARIZER_MODEL,
        ] {
            assert!(
                slug.starts_with("anthropic/claude-"),
                "{slug} is not an OpenRouter anthropic slug"
            );
            assert!(
                slug.contains('.'),
                "{slug} uses the dashed spelling; OpenRouter names versions with a dot"
            );
        }
        assert!(DEFAULT_REVIEWER_MODEL.contains("sonnet"));
        assert!(DEFAULT_VERIFIER_MODEL.contains("haiku"));
        assert!(DEFAULT_SUMMARIZER_MODEL.contains("haiku"));
    }

    #[test]
    fn a_models_table_beats_the_built_in_slugs() {
        let text = format!(
            "{CONFIG}\n[models]\nreviewer = \"anthropic/claude-opus-4.8\"\nprovider = \"bedrock\"\n"
        );
        let env = pairs(&config_from(&text));
        assert_eq!(
            env.get(ENV_REVIEWER_MODEL).map(String::as_str),
            Some("anthropic/claude-opus-4.8")
        );
        assert_eq!(env.get(ENV_PROVIDER).map(String::as_str), Some("bedrock"));
        // Roles the table did not name still get the defaults.
        assert_eq!(
            env.get(ENV_VERIFIER_MODEL).map(String::as_str),
            Some(DEFAULT_VERIFIER_MODEL)
        );
    }

    /// The precedence rule that matters most: a value the operator already
    /// exported is inherited, never replaced.
    #[test]
    fn an_operator_variable_is_never_overridden() {
        let text = format!("{CONFIG}\n[models]\nreviewer = \"anthropic/claude-opus-4.8\"\n");
        let config = config_from(&text);
        let env: HashMap<&str, String> = inference_env(&config, |name| {
            (name == ENV_PROVIDER || name == ENV_REVIEWER_MODEL).then(|| "operator".to_owned())
        })
        .into_iter()
        .collect();

        assert!(
            !env.contains_key(ENV_PROVIDER),
            "an operator-set provider must be inherited, not clobbered: {env:?}"
        );
        assert!(
            !env.contains_key(ENV_REVIEWER_MODEL),
            "an operator-set model must outrank the engagement config: {env:?}"
        );
        // The variables the operator said nothing about are still supplied.
        assert_eq!(
            env.get(ENV_VERIFIER_MODEL).map(String::as_str),
            Some(DEFAULT_VERIFIER_MODEL)
        );
        assert_eq!(
            env.get(ENV_SUMMARIZER_MODEL).map(String::as_str),
            Some(DEFAULT_SUMMARIZER_MODEL)
        );
    }

    /// No credential, no provider selection — pointing the reviewer at
    /// OpenRouter with nothing to authenticate with just moves the failure.
    #[test]
    fn a_blank_key_selects_nothing() {
        let text = CONFIG.replace("sk-or-v1-not-a-real-key", "   ");
        assert!(inference_env(&config_from(&text), nothing_set).is_empty());
    }
}
