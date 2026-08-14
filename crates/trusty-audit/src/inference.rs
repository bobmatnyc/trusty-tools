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
//! ## The four variables are ONE selection, not four
//!
//! A model id only means anything inside its provider's namespace:
//! `anthropic/claude-sonnet-4.6` is an OpenRouter slug and
//! `us.anthropic.claude-sonnet-4-6` is a Bedrock inference profile for the same
//! model. So provider and models are resolved as a UNIT, from exactly one
//! layer — highest first:
//!
//! 1. The operator's process environment.
//! 2. The engagement config's `[models]` table ([`ModelPins`]).
//! 3. The built-in slugs below.
//!
//! **The first layer that names any of the four must name all four**, or
//! [`inference_env`] returns [`AuditError::SplitInferenceSelection`] and the
//! sweep refuses before spawning anything. Resolving the four independently is
//! what reproduces #5671: an operator who exports
//! `TRUSTY_REVIEW_PROVIDER=bedrock` and nothing else would otherwise have
//! `bedrock` paired with this module's OpenRouter slugs — the same HTTP 400
//! this module exists to remove, arrived at from the other direction. Nothing
//! downstream catches that pairing: `resolve_role` resolves provider and model
//! on independent chains too (#5679).
//!
//! Refusing is deliberate. The alternative is guessing which half someone meant,
//! and a wrong guess here is billed to the operator in a currency they did not
//! choose.
//!
//! Test: `super::inference_tests`, and `crate::run::run_tests`'
//! `the_child_environment_selects_openrouter_and_all_three_models`,
//! `a_fully_set_operator_environment_is_left_alone`,
//! `a_partial_operator_environment_refuses_before_any_child_runs`.

use crate::config::{EngagementConfig, ModelPins};
use crate::error::AuditError;

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

/// The four variables in the order they are reported, so an error message and
/// the emitted pairs list read the same way.
const SELECTION: [&str; 4] = [
    ENV_PROVIDER,
    ENV_REVIEWER_MODEL,
    ENV_VERIFIER_MODEL,
    ENV_SUMMARIZER_MODEL,
];

/// Split a layer's four values into (named, unnamed), preserving order.
fn partition(
    values: [Option<&str>; 4],
    names: [&'static str; 4],
) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut named = Vec::new();
    let mut unnamed = Vec::new();
    for (value, name) in values.into_iter().zip(names) {
        if value.is_some() {
            named.push(name)
        } else {
            unnamed.push(name)
        }
    }
    (named, unnamed)
}

/// Reject a layer that named some of the four but not all of them.
fn all_or_none(
    values: [Option<&str>; 4],
    names: [&'static str; 4],
    layer: &'static str,
) -> Result<bool, AuditError> {
    let (named, missing) = partition(values, names);
    match named.len() {
        0 => Ok(false),
        4 => Ok(true),
        _ => Err(AuditError::SplitInferenceSelection {
            layer,
            set: named.join(", "),
            missing: missing.join(", "),
        }),
    }
}

/// Build the `TRUSTY_REVIEW_*` pairs to set on the `tga audit` child.
///
/// # Postconditions
///
/// On `Ok`, the result is either EMPTY or names all four variables — never a
/// subset, which is the property that keeps a provider from being paired with
/// another provider's model ids. Empty means the child's inherited environment
/// is already self-consistent (the operator named all four) or there is nothing
/// to authenticate with (a blank engagement key). Otherwise all four come from
/// the same layer: the engagement `[models]` table if it named them, else the
/// built-in slugs, with [`ENV_PROVIDER`] set to [`PROVIDER_OPENROUTER`].
///
/// # Errors
///
/// [`AuditError::SplitInferenceSelection`] when the operator's environment or
/// the `[models]` table names some of the four but not all four. See the module
/// docs for why that refuses rather than filling in the rest.
///
/// `operator` is a parameter rather than a direct `std::env::var` call for the
/// same reason [`crate::workdir::WorkDir::resolve`] takes the environment: the
/// precedence rule is then provable — through the real spawn path, not just a
/// unit test — without mutating the process environment, which is racy across a
/// parallel test binary and `unsafe` in edition 2024.
/// Test: `super::inference_tests::an_operator_who_named_all_four_is_left_alone`,
/// `super::inference_tests::a_partly_set_operator_environment_refuses`.
pub fn inference_env<F>(
    config: &EngagementConfig,
    operator: F,
) -> Result<Vec<(&'static str, String)>, AuditError>
where
    F: Fn(&str) -> Option<String>,
{
    if config.openrouter_key.is_empty() {
        return Ok(Vec::new());
    }

    let from_operator = SELECTION.map(&operator);
    if all_or_none(
        [
            from_operator[0].as_deref(),
            from_operator[1].as_deref(),
            from_operator[2].as_deref(),
            from_operator[3].as_deref(),
        ],
        SELECTION,
        "operator environment",
    )? {
        // The operator named the whole selection; the child inherits it intact.
        return Ok(Vec::new());
    }

    let pins: &ModelPins = &config.models;
    let from_config = [
        pins.provider.as_deref(),
        pins.reviewer.as_deref(),
        pins.verifier.as_deref(),
        pins.summarizer.as_deref(),
    ];
    let config_names = ["provider", "reviewer", "verifier", "summarizer"];
    let use_config = all_or_none(
        from_config,
        config_names,
        "engagement config `[models]` table",
    )?;

    let defaults = [
        PROVIDER_OPENROUTER,
        DEFAULT_REVIEWER_MODEL,
        DEFAULT_VERIFIER_MODEL,
        DEFAULT_SUMMARIZER_MODEL,
    ];
    Ok(SELECTION
        .into_iter()
        .zip(from_config)
        .zip(defaults)
        .map(|((name, pinned), fallback)| {
            let value = if use_config {
                pinned.unwrap_or(fallback)
            } else {
                fallback
            };
            (name, value.to_owned())
        })
        .collect())
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

    /// A `[models]` table naming the whole selection, which is the only shape
    /// the all-or-none rule accepts from the config layer.
    const WHOLE_TABLE: &str = "\n[models]\nprovider = \"openrouter\"\n\
         reviewer = \"anthropic/claude-opus-4.8\"\nverifier = \"anthropic/claude-haiku-4.5\"\n\
         summarizer = \"anthropic/claude-haiku-4.5\"\n";

    fn config_from(text: &str) -> EngagementConfig {
        EngagementConfig::from_toml(text, Path::new("engagement.toml")).expect("parses")
    }

    fn nothing_set(_: &str) -> Option<String> {
        None
    }

    fn pairs(config: &EngagementConfig) -> HashMap<&'static str, String> {
        inference_env(config, nothing_set)
            .expect("an untouched operator environment resolves")
            .into_iter()
            .collect()
    }

    #[test]
    fn the_defaults_select_openrouter_and_all_three_roles() {
        let env = pairs(&config_from(CONFIG));
        assert_eq!(
            env.get(ENV_PROVIDER).map(String::as_str),
            Some(PROVIDER_OPENROUTER)
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
    fn a_whole_models_table_beats_the_built_in_slugs() {
        let env = pairs(&config_from(&format!("{CONFIG}{WHOLE_TABLE}")));
        assert_eq!(
            env.get(ENV_REVIEWER_MODEL).map(String::as_str),
            Some("anthropic/claude-opus-4.8")
        );
        assert_eq!(
            env.get(ENV_PROVIDER).map(String::as_str),
            Some(PROVIDER_OPENROUTER)
        );
    }

    /// The mirror of the operator rule, at the config layer: naming a model
    /// without its provider (or the reverse) is the same unresolvable half-ask.
    #[test]
    fn a_partly_filled_models_table_refuses() {
        let text = format!("{CONFIG}\n[models]\nreviewer = \"anthropic/claude-opus-4.8\"\n");
        let err = inference_env(&config_from(&text), nothing_set)
            .expect_err("half a selection is not a selection");
        let AuditError::SplitInferenceSelection {
            layer,
            set,
            missing,
        } = &err
        else {
            panic!("expected SplitInferenceSelection, got {err:?}");
        };
        assert!(layer.contains("engagement config"), "{layer}");
        assert_eq!(set, "reviewer");
        assert_eq!(missing, "provider, verifier, summarizer");
    }

    /// An operator who named the whole selection owns it: emit nothing, so the
    /// child inherits their four values rather than a mix of theirs and ours.
    #[test]
    fn an_operator_who_named_all_four_is_left_alone() {
        let text = format!("{CONFIG}{WHOLE_TABLE}");
        let emitted = inference_env(&config_from(&text), |_| Some("operator".to_owned()))
            .expect("a whole operator selection resolves");
        assert!(
            emitted.is_empty(),
            "a complete operator selection must be inherited untouched: {emitted:?}"
        );
    }

    /// The #5671-class regression, arrived at from the other direction: an
    /// operator on Bedrock who sets only the provider must not be handed
    /// OpenRouter slugs to pair with it.
    #[test]
    fn a_partly_set_operator_environment_refuses() {
        let err = inference_env(&config_from(CONFIG), |name| {
            (name == ENV_PROVIDER).then(|| "bedrock".to_owned())
        })
        .expect_err("a provider without models is a mismatch waiting to happen");
        let AuditError::SplitInferenceSelection {
            layer,
            set,
            missing,
        } = &err
        else {
            panic!("expected SplitInferenceSelection, got {err:?}");
        };
        assert_eq!(*layer, "operator environment");
        assert_eq!(set, ENV_PROVIDER);
        assert_eq!(
            missing,
            "TRUSTY_REVIEW_REVIEWER_MODEL, TRUSTY_REVIEW_VERIFIER_MODEL, \
             TRUSTY_REVIEW_SUMMARIZER_MODEL"
        );
        // The message has to be actionable without reading this crate.
        let rendered = err.to_string();
        assert!(rendered.contains("all four or none"), "{rendered}");
    }

    /// The mirror case: a model named without a provider must not have
    /// `openrouter` forced under it.
    #[test]
    fn an_operator_who_set_only_a_model_refuses() {
        let err = inference_env(&config_from(CONFIG), |name| {
            (name == ENV_REVIEWER_MODEL).then(|| "us.anthropic.claude-sonnet-4-6".to_owned())
        })
        .expect_err("a model without a provider is the same half-ask");
        assert!(
            matches!(err, AuditError::SplitInferenceSelection { .. }),
            "{err:?}"
        );
    }

    /// No credential, no provider selection — pointing the reviewer at
    /// OpenRouter with nothing to authenticate with just moves the failure.
    #[test]
    fn a_blank_key_selects_nothing() {
        let text = CONFIG.replace("sk-or-v1-not-a-real-key", "   ");
        assert!(
            inference_env(&config_from(&text), nothing_set)
                .expect("a blank key is not an error")
                .is_empty()
        );
    }

    /// The property the whole module exists for: whatever the inputs, the
    /// result never names a strict subset of the four.
    #[test]
    fn the_result_is_never_a_partial_selection() {
        let cases = [CONFIG.to_owned(), format!("{CONFIG}{WHOLE_TABLE}")];
        for text in cases {
            for operator_set_all in [false, true] {
                let emitted = inference_env(&config_from(&text), |_| {
                    operator_set_all.then(|| "operator".to_owned())
                })
                .expect("resolves");
                assert!(
                    emitted.is_empty() || emitted.len() == SELECTION.len(),
                    "emitted a partial selection: {emitted:?}"
                );
            }
        }
    }
}
