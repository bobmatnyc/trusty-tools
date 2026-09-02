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

/// Reviewer model — the audit's judging call, so the Opus analysis tier.
///
/// Why: owner ruling, 2026-08-18 — for `trusty-audit` the analysis tier is Opus
/// 4.8. It was `anthropic/claude-sonnet-4.6`, which paired this crate's judging
/// call with the Sonnet-class split `trusty-review` uses for Bedrock. This
/// constant is the BOTTOM of `trusty-review`'s precedence chain (CLI flag >
/// environment > config file > built-in), so it decides only for an engagement
/// config that names no `[models]` table — which is the auditor-supplied
/// handoff, the common case. Leaving it would have applied the ruling to
/// configs `crate::cli::bootstrap` writes and to nothing else (#5970).
/// What: OpenRouter names Anthropic models `anthropic/claude-<tier>-<major>.<minor>`
/// — a DOT, not a dash. Verified against `GET https://openrouter.ai/api/v1/models`
/// on 2026-08-18, which lists `anthropic/claude-opus-4.8` and no dashed
/// `-4-8` form. The dashed spelling in `trusty-agents` is that crate's own
/// convention and is not an OpenRouter slug; sending it produces the HTTP 400
/// `is not a valid model ID` this constant exists to avoid.
/// Test: `super::inference_tests::a_config_with_no_models_table_judges_on_opus`.
pub const DEFAULT_REVIEWER_MODEL: &str = "anthropic/claude-opus-4.8";

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
    selection_env(!config.openrouter_key.is_empty(), &config.models, operator)
}

/// [`inference_env`], for a caller that has a key but may have no config.
///
/// Why: #6080's re-render runs on the machine of whoever received the finished
/// audit, and that machine has no `engagement.toml` — the config travels TO a
/// recipient, not back out with the deliverable. The selection rule is the same
/// one either way, so it stays one function rather than a second copy that can
/// drift about which layer wins; only where the two inputs come from differs.
/// What: everything [`inference_env`] does, with the key's presence and the
/// `[models]` table passed in. A caller with no config passes
/// [`ModelPins::default`], which resolves the built-in slugs.
/// Test: `super::inference_tests::an_absent_models_table_resolves_the_defaults`.
///
/// # Errors
///
/// Exactly [`inference_env`]'s.
pub fn selection_env<F>(
    key_present: bool,
    models: &ModelPins,
    operator: F,
) -> Result<Vec<(&'static str, String)>, AuditError>
where
    F: Fn(&str) -> Option<String>,
{
    Ok(resolve(key_present, models, operator)?.env)
}

/// A run's inference identity: the provider and the three role model ids.
///
/// Why: #6135 — the identity must be written into the manifest, and the env
/// pairs alone cannot carry it. They are EMPTY in the arm where the operator
/// named all four, which is exactly the arm where the manifest most needs to
/// record what was used. Owner ruling 2026-08-21: "In audit mode, trusty review
/// should use the same provider as audit to make it portable. From the
/// manifest."
/// What: four values, always all four, whichever layer they came from.
/// `source` names that layer so the index can state it.
/// Test: `super::inference_tests::the_manifest_carries_what_the_env_carries`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Selection {
    /// Provider id — `openrouter` unless the engagement pinned another.
    pub provider: String,
    /// Reviewer role model id.
    pub reviewer: String,
    /// Verifier role model id.
    pub verifier: String,
    /// Summarizer role model id.
    pub summarizer: String,
    /// Which layer resolved it: the operator's environment, the engagement
    /// config's `[models]` table, or the built-in defaults.
    pub source: &'static str,
}

impl Selection {
    /// The four values in `[inference]` key order.
    #[must_use]
    pub fn rows(&self) -> [(&'static str, &str); 4] {
        [
            ("provider", self.provider.as_str()),
            ("reviewer", self.reviewer.as_str()),
            ("verifier", self.verifier.as_str()),
            ("summarizer", self.summarizer.as_str()),
        ]
    }
}

/// What one run selected, and what it must hand to a child.
///
/// Why: the two are not the same value. `env` is empty when the operator's
/// environment already names all four — the child inherits it intact — while
/// `selection` is what that environment resolved TO, which is what the manifest
/// records. Returning both from one call keeps them from being resolved twice
/// and disagreeing.
/// What: `selection` is `None` only when there is no credential, in which case
/// nothing is selected and nothing is injected.
/// Test: `super::inference_tests::the_manifest_carries_what_the_env_carries`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Inference {
    /// The `TRUSTY_REVIEW_*` pairs to set on a child — all four, or none.
    pub env: Vec<(&'static str, String)>,
    /// The identity this run renders with, when it has one.
    pub selection: Option<Selection>,
}

/// Resolve both halves of a run's inference identity.
///
/// # Errors
///
/// Exactly [`inference_env`]'s.
///
/// Test: `super::inference_tests::the_manifest_carries_what_the_env_carries`.
pub fn resolve<F>(
    key_present: bool,
    models: &ModelPins,
    operator: F,
) -> Result<Inference, AuditError>
where
    F: Fn(&str) -> Option<String>,
{
    if !key_present {
        return Ok(Inference::default());
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
        // The operator named the whole selection; the child inherits it intact,
        // and the manifest records what that selection actually is (#6135).
        let named = |i: usize| from_operator[i].clone().unwrap_or_default();
        return Ok(Inference {
            env: Vec::new(),
            selection: Some(Selection {
                provider: named(0),
                reviewer: named(1),
                verifier: named(2),
                summarizer: named(3),
                source: "the operator's environment",
            }),
        });
    }

    let pins: &ModelPins = models;
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
    let values: Vec<String> = from_config
        .into_iter()
        .zip(defaults)
        .map(|(pinned, fallback)| {
            if use_config {
                pinned.unwrap_or(fallback).to_owned()
            } else {
                fallback.to_owned()
            }
        })
        .collect();
    let env: Vec<(&'static str, String)> =
        SELECTION.into_iter().zip(values.iter().cloned()).collect();
    Ok(Inference {
        selection: Some(Selection {
            provider: values[0].clone(),
            reviewer: values[1].clone(),
            verifier: values[2].clone(),
            summarizer: values[3].clone(),
            source: if use_config {
                "the engagement config's [models] table"
            } else {
                "this tool's built-in defaults"
            },
        }),
        env,
    })
}

/// Record `selection` in the manifest at `path` as its `[inference]` table.
///
/// Why: the manifest is what ships. `trusty-review` resolves provider and models
/// from it ahead of the host's own `~/.config/trusty-review/config.toml`, so
/// writing it here is what makes a delivered package re-render on the provider
/// that produced it rather than on whatever the recipient's machine is pinned to
/// (#6135, and #6080's portable re-render is the use case).
/// What: an `[inference]` table with the four identity keys, replacing any
/// previous one. NEVER a credential — a key stays in the environment, because
/// this file is handed to the client.
///
/// The write is the same read-parse-write shape
/// [`crate::grounding::priority::write_into`] uses on the same file, and runs
/// after it for that reason: `toml_edit` preserves what it did not touch, so the
/// two writers cannot erase each other.
///
/// # Errors
///
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, or written back. The caller turns it into a gap of its own — a
/// manifest without the section still renders, it just resolves its provider
/// from the environment as it did before this key existed.
///
/// Test: `super::inference_tests::{the_section_lands_in_the_manifest,
/// a_manifest_that_cannot_be_written_says_so}`.
pub fn write_into_manifest(path: &std::path::Path, selection: &Selection) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} could not be read ({e})", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not readable as TOML ({e})", path.display()))?;

    let mut table = toml_edit::Table::new();
    for (key, value) in selection.rows() {
        table.insert(key, toml_edit::value(value));
    }
    doc.insert("inference", toml_edit::Item::Table(table));

    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", path.display()))
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
trusty-search = "0.47.0"
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

    /// #6080: the re-render runs where there is no `engagement.toml` at all, so
    /// the selection has to resolve from a key and nothing else — and it must be
    /// the same four values the sweep would have used.
    #[test]
    fn an_absent_models_table_resolves_the_defaults() {
        let without: HashMap<&'static str, String> =
            selection_env(true, &ModelPins::default(), nothing_set)
                .expect("a key with no config resolves")
                .into_iter()
                .collect();
        assert_eq!(without, pairs(&config_from(CONFIG)));
        assert_eq!(
            without.get(ENV_PROVIDER).map(String::as_str),
            Some(PROVIDER_OPENROUTER)
        );
        assert!(
            selection_env(false, &ModelPins::default(), nothing_set)
                .expect("no key resolves")
                .is_empty(),
            "with nothing to authenticate with there is nothing to select"
        );
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
        // #5970: the reviewer is the analysis tier, per the owner's ruling of
        // 2026-08-18. It was `sonnet` until then.
        assert!(DEFAULT_REVIEWER_MODEL.contains("opus"));
        assert!(DEFAULT_VERIFIER_MODEL.contains("haiku"));
        assert!(DEFAULT_SUMMARIZER_MODEL.contains("haiku"));
    }

    /// 🔴 The default path, asserted as LITERALS.
    ///
    /// Owner ruling, 2026-08-18: for `trusty-audit` the analysis tier is Opus
    /// 4.8. An engagement config with no `[models]` table — the auditor-supplied
    /// handoff, and the common case — resolves through the built-in constants,
    /// so this is the path the ruling reaches only via [`DEFAULT_REVIEWER_MODEL`].
    ///
    /// It is distinct from `the_defaults_select_openrouter_and_all_three_roles`
    /// above, which compares the pairs to those same constants and therefore
    /// passes whatever they happen to say — including a Bedrock
    /// `us.anthropic.…` profile id named against an OpenRouter endpoint, which
    /// fails at call time rather than at compile time. Only a literal catches
    /// that. Same reason
    /// `crate::cli::bootstrap::bootstrap_tests::the_written_config_names_the_ruled_models`
    /// spells its ids out; that test covers the config-file layer, this one the
    /// built-in layer underneath it.
    #[test]
    fn a_config_with_no_models_table_judges_on_opus() {
        let config = config_from(CONFIG);
        assert_eq!(
            config.models,
            ModelPins::default(),
            "the fixture must name no models"
        );

        let env = pairs(&config);
        assert_eq!(
            env.get(ENV_PROVIDER).map(String::as_str),
            Some("openrouter")
        );
        assert_eq!(
            env.get(ENV_REVIEWER_MODEL).map(String::as_str),
            Some("anthropic/claude-opus-4.8"),
            "an auditor-supplied config must judge on Opus 4.8: {env:?}"
        );
        assert_eq!(
            env.get(ENV_VERIFIER_MODEL).map(String::as_str),
            Some("anthropic/claude-haiku-4.5"),
            "{env:?}"
        );
        assert_eq!(
            env.get(ENV_SUMMARIZER_MODEL).map(String::as_str),
            Some("anthropic/claude-haiku-4.5"),
            "{env:?}"
        );
        assert!(
            env.values().all(|v| !v.starts_with("us.anthropic.")),
            "a Bedrock inference-profile id must never reach an OpenRouter run: {env:?}"
        );
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
    /// Why: #6135 — the manifest is only a portable carrier if it says exactly
    /// what the environment says. A drift between the two would render one
    /// report on one provider and its re-render on another.
    /// What: resolves both halves and asserts the four `[inference]` values are
    /// the four `TRUSTY_REVIEW_*` values, in the same pairing.
    /// Test: this test itself.
    #[test]
    fn the_manifest_carries_what_the_env_carries() {
        let inference = resolve(true, &ModelPins::default(), nothing_set).expect("resolves");
        let env: HashMap<&'static str, String> = inference.env.iter().cloned().collect();
        let selection = inference.selection.expect("a key selects something");

        assert_eq!(env.get(ENV_PROVIDER), Some(&selection.provider));
        assert_eq!(env.get(ENV_REVIEWER_MODEL), Some(&selection.reviewer));
        assert_eq!(env.get(ENV_VERIFIER_MODEL), Some(&selection.verifier));
        assert_eq!(env.get(ENV_SUMMARIZER_MODEL), Some(&selection.summarizer));
        assert_eq!(selection.provider, PROVIDER_OPENROUTER);
    }

    /// Why: the arm where the operator named all four emits NO variables, and
    /// that is exactly the arm where the manifest is the only record of what ran.
    /// What: an operator environment naming all four, resolved.
    /// Test: this test itself.
    #[test]
    fn an_inherited_selection_is_still_recorded() {
        let inference = resolve(true, &ModelPins::default(), |name| {
            Some(match name {
                ENV_PROVIDER => "bedrock".to_owned(),
                _ => "us.anthropic.claude-sonnet-4-6".to_owned(),
            })
        })
        .expect("a whole operator selection resolves");

        assert!(inference.env.is_empty(), "the child inherits it untouched");
        let selection = inference
            .selection
            .expect("but the manifest still records it");
        assert_eq!(selection.provider, "bedrock");
        assert_eq!(selection.reviewer, "us.anthropic.claude-sonnet-4-6");
    }

    /// Why: the section is what makes a delivered package render on the provider
    /// that produced it, so it has to actually land in the file — and land once,
    /// however many times a resumed sweep writes it.
    /// What: writes into a manifest twice and reads the result back as TOML.
    /// Test: this test itself.
    #[test]
    fn the_section_lands_in_the_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(
            &path,
            "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"api\"\npath = \"/r\"\n",
        )
        .expect("write");

        let selection = resolve(true, &ModelPins::default(), nothing_set)
            .expect("resolves")
            .selection
            .expect("selects");
        write_into_manifest(&path, &selection).expect("the section is written");
        write_into_manifest(&path, &selection).expect("a resumed sweep writes it again");

        let text = std::fs::read_to_string(&path).expect("read back");
        let doc: toml::Value = toml::from_str(&text).expect("still parses as TOML");
        let section = doc.get("inference").expect("the section is there");
        assert_eq!(
            section.get("provider").and_then(toml::Value::as_str),
            Some(PROVIDER_OPENROUTER)
        );
        assert_eq!(
            section.get("reviewer").and_then(toml::Value::as_str),
            Some(DEFAULT_REVIEWER_MODEL)
        );
        assert_eq!(
            text.matches("[inference]").count(),
            1,
            "a second write replaces the section rather than appending one: {text}"
        );
        assert_eq!(
            doc.get("report")
                .and_then(|r| r.get("title"))
                .and_then(toml::Value::as_str),
            Some("Acme"),
            "everything the other writer put here survives"
        );
        assert!(
            !text.contains("key") && !text.contains("sk-or"),
            "a credential must never reach a file the client receives: {text}"
        );
    }

    /// Why: a manifest that cannot be written is a degraded render, never a
    /// failed sweep — the caller turns this into a named gap.
    /// What: writes into a path that does not exist.
    /// Test: this test itself.
    #[test]
    fn a_manifest_that_cannot_be_written_says_so() {
        let selection = resolve(true, &ModelPins::default(), nothing_set)
            .expect("resolves")
            .selection
            .expect("selects");
        let cause = write_into_manifest(Path::new("/nonexistent/manifest.toml"), &selection)
            .expect_err("an absent manifest cannot be written");
        assert!(cause.contains("could not be read"), "{cause}");
    }

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
