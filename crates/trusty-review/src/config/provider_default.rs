//! Built-in default provider resolution from available credentials (#5671).
//!
//! Why: `RoleModels::resolve` hard-coded `Provider::Bedrock` as the built-in
//! default for every role, so an operator who supplied only `OPENROUTER_API_KEY`
//! reached Bedrock and failed — on a client machine with no AWS credentials the
//! run died, and on one that had them it billed the wrong account. The audit
//! deliverable ships a spend-capped OpenRouter key (`trusty-audit`'s required
//! `EngagementConfig::openrouter_key`), so the key must be reachable without
//! anyone exporting `TRUSTY_REVIEW_PROVIDER` by hand.
//! What: [`CredentialEnv`] detects which credentials are usable, and
//! [`ProviderDefault::detect`] turns that into a provider plus the matching
//! per-role default model ids. Detection never falls back to a provider it
//! could not find a credential for — the no-credential case is reported as
//! [`ProviderSource::NoCredential`] and the run fails at provider construction
//! with a message naming both options.
//! Test: `provider_default_*` unit tests below cover every precedence branch,
//! blank-key rejection, and the no-credential case.

use std::path::PathBuf;

use super::Provider;

/// OpenRouter reviewer default — the Sonnet-tier slug used elsewhere in this
/// workspace (`trusty-agents`' `qualify_openrouter_model`).
pub const DEFAULT_OPENROUTER_REVIEWER_MODEL: &str = "anthropic/claude-sonnet-4-6";
/// OpenRouter verifier default — Haiku-tier, matching the Bedrock verifier's
/// cost/latency profile.
pub const DEFAULT_OPENROUTER_VERIFIER_MODEL: &str = "anthropic/claude-haiku-4-5";
/// OpenRouter summarizer default — same Haiku-tier slug as the verifier.
pub const DEFAULT_OPENROUTER_SUMMARIZER_MODEL: &str = "anthropic/claude-haiku-4-5";

/// Which precedence rule chose the built-in default provider.
///
/// Why: `build_provider` must be able to refuse a run whose provider was never
/// actually chosen — a resolver that silently picked one anyway is the
/// fail-open shape #5671 exists to remove. Carrying the source alongside the
/// resolved provider is what makes that refusal possible without making
/// `ReviewConfig` construction fallible.
/// What: a four-variant marker; only [`ProviderSource::NoCredential`] blocks a
/// run, and only when the role's model carries no explicit provider prefix.
/// Test: `provider_default_no_credential_is_reported`,
/// `role_models_no_credential_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderSource {
    /// A CLI flag, `TRUSTY_REVIEW_PROVIDER`, or the config file named a
    /// provider. Always wins; no credential detection runs.
    Explicit,
    /// A non-blank `OPENROUTER_API_KEY` was present.
    OpenRouterKey,
    /// AWS credentials were detectable in the environment or `~/.aws`.
    AwsCredentials,
    /// Neither was found. The run must fail naming both options rather than
    /// defaulting to a provider that will fail obscurely.
    NoCredential,
}

/// Credentials detected in the process environment.
///
/// Why: detection has to be injectable so the precedence tests do not mutate
/// the shared process environment (which races across test binaries), and so
/// no global cached decision is needed — the workspace forbids `Lazy` state.
/// What: a plain value carrying whether each provider's credential is usable.
/// `openrouter` is *usable*, not merely *present*: a blank or whitespace-only
/// `OPENROUTER_API_KEY` does not win precedence and then fail at the API
/// boundary (the `GH_TOKEN="   "` class of mistake `trusty_common::gh`'s
/// `nonempty_stdout` guards against).
/// Test: `credential_env_rejects_blank_openrouter_key`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CredentialEnv {
    /// A non-blank `OPENROUTER_API_KEY` is set.
    pub openrouter: bool,
    /// AWS credentials look resolvable.
    pub aws: bool,
}

impl CredentialEnv {
    /// Detect credentials from the process environment.
    ///
    /// Why: the one place that reads credential env vars for provider
    /// selection, so the rules cannot drift between call sites.
    /// What: reads `OPENROUTER_API_KEY` (trimmed, must be non-empty) and the
    /// standard AWS credential env vars, falling back to the presence of
    /// `~/.aws/credentials` or `~/.aws/config`.
    ///
    /// Known limit: EC2/ECS instance-metadata credentials are only detected
    /// via the container-credential env vars. A machine whose sole credential
    /// source is IMDS resolves as `aws: false`; set `TRUSTY_REVIEW_PROVIDER=bedrock`
    /// there. Probing IMDS would require a network round-trip inside a
    /// synchronous config load.
    /// Test: exercised indirectly; the pure logic is tested via `detect`.
    pub fn from_env() -> Self {
        Self {
            openrouter: nonblank_var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY),
            aws: aws_credentials_detected(),
        }
    }
}

/// True when `name` is set to a value with at least one non-whitespace byte.
///
/// Why: `#5671` — a blank key must not win precedence and then fail.
/// What: `std::env::var` plus a `trim().is_empty()` check.
/// Test: `credential_env_rejects_blank_openrouter_key`.
fn nonblank_var(name: &str) -> bool {
    is_usable(std::env::var(name).ok().as_deref())
}

/// True when an env-var value is present and not whitespace-only.
///
/// Why: split out from [`nonblank_var`] so the rule is testable without
/// mutating the shared process environment.
/// What: `Some(v)` with a non-empty `v.trim()`.
/// Test: `credential_env_rejects_blank_openrouter_key`.
fn is_usable(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// Heuristic AWS-credential detection.
///
/// Why: Bedrock must keep working unchanged for operators who already have AWS
/// credentials and no OpenRouter key.
/// What: static keys, a named profile, web-identity role assumption, container
/// credentials, or a `~/.aws` config/credentials file.
/// Test: covered by the `provider_default_*` tests through injected
/// `CredentialEnv` values; this function itself reads the real environment.
fn aws_credentials_detected() -> bool {
    if (nonblank_var("AWS_ACCESS_KEY_ID") && nonblank_var("AWS_SECRET_ACCESS_KEY"))
        || nonblank_var("AWS_PROFILE")
        || nonblank_var("AWS_WEB_IDENTITY_TOKEN_FILE")
        || nonblank_var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
        || nonblank_var("AWS_CONTAINER_CREDENTIALS_FULL_URI")
    {
        return true;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    let aws = home.join(".aws");
    aws.join("credentials").is_file() || aws.join("config").is_file()
}

/// The resolved built-in default: a provider plus its per-role model ids.
///
/// Why: provider and model default are one decision, not two — the built-in
/// model ids are Bedrock inference-profile ids, so flipping the provider to
/// OpenRouter without flipping the models would send `us.anthropic.*` to
/// OpenRouter and fail.
/// What: the chosen provider, why it was chosen, and the three role model ids
/// that go with it.
/// Test: `provider_default_openrouter_key_wins`,
/// `provider_default_aws_only_is_bedrock`, `provider_default_explicit_wins`.
#[derive(Debug, Clone)]
pub struct ProviderDefault {
    /// Provider to use when no layer named one.
    pub provider: Provider,
    /// Which precedence rule chose it.
    pub source: ProviderSource,
    /// Built-in reviewer model id for `provider`.
    pub reviewer_model: &'static str,
    /// Built-in verifier model id for `provider`.
    pub verifier_model: &'static str,
    /// Built-in summarizer model id for `provider`.
    pub summarizer_model: &'static str,
}

impl ProviderDefault {
    /// Apply the precedence chain.
    ///
    /// Why: #5671 requires the OpenRouter key to be reached without an explicit
    /// override, while leaving an AWS-only setup exactly as it was.
    /// What: precedence is
    /// 1. `explicit` (CLI flag / `TRUSTY_REVIEW_PROVIDER` / config file) — always wins;
    /// 2. a usable `OPENROUTER_API_KEY` → OpenRouter;
    /// 3. resolvable AWS credentials → Bedrock;
    /// 4. neither → [`ProviderSource::NoCredential`], which fails at provider
    ///    construction naming both options.
    ///
    /// Rule 2 outranks rule 3 deliberately: a supplied, spend-capped key is a
    /// stronger statement of intent than ambient AWS credentials, and the
    /// engagement config that supplies it has no other way to be honoured.
    /// Test: `provider_default_explicit_wins`, `provider_default_openrouter_key_wins`,
    /// `provider_default_aws_only_is_bedrock`, `provider_default_no_credential_is_reported`.
    pub fn detect(explicit: Option<Provider>, creds: &CredentialEnv) -> Self {
        if let Some(provider) = explicit {
            return Self::for_provider(provider, ProviderSource::Explicit);
        }
        if creds.openrouter {
            return Self::for_provider(Provider::OpenRouter, ProviderSource::OpenRouterKey);
        }
        if creds.aws {
            return Self::for_provider(Provider::Bedrock, ProviderSource::AwsCredentials);
        }
        // No fallback provider: the models are irrelevant because
        // `build_provider` refuses the run before using them. Bedrock's ids are
        // kept so anything that merely displays the config reads as it did.
        Self::for_provider(Provider::Bedrock, ProviderSource::NoCredential)
    }

    /// Model ids that go with a provider.
    ///
    /// Why: keeps the provider→model pairing in one place.
    /// What: OpenRouter uses vendor-namespaced slugs; Bedrock and Fireworks
    /// keep the `llm::models` ids they already had — a `--provider fireworks`
    /// run picked those before #5671 and still does, since Fireworks is only
    /// ever reachable explicitly and its operator names their own model.
    /// Test: `provider_default_openrouter_models_are_slugs`.
    fn for_provider(provider: Provider, source: ProviderSource) -> Self {
        match provider {
            Provider::OpenRouter => Self {
                provider,
                source,
                reviewer_model: DEFAULT_OPENROUTER_REVIEWER_MODEL,
                verifier_model: DEFAULT_OPENROUTER_VERIFIER_MODEL,
                summarizer_model: DEFAULT_OPENROUTER_SUMMARIZER_MODEL,
            },
            Provider::Bedrock | Provider::Fireworks => Self {
                provider,
                source,
                reviewer_model: crate::llm::models::DEFAULT_REVIEWER_MODEL,
                verifier_model: crate::llm::models::DEFAULT_VERIFIER_MODEL,
                summarizer_model: crate::llm::models::DEFAULT_SUMMARIZER_MODEL,
            },
        }
    }
}

/// Operator-facing message for the no-credential case.
///
/// Why: the damage in #5671 was not the wrong default but that the failure
/// named Bedrock when the operator had supplied an OpenRouter key and had no
/// idea why AWS was involved. This message names both options and the override.
/// What: a single static string used by `llm::build_provider`.
/// Test: `provider_default_message_names_both_options`.
pub const NO_CREDENTIAL_MESSAGE: &str = "no LLM credential found: set OPENROUTER_API_KEY to use OpenRouter, or configure AWS \
     credentials (AWS_PROFILE, AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, or ~/.aws) to use \
     Bedrock. To force a provider regardless of detection, set TRUSTY_REVIEW_PROVIDER=openrouter \
     or TRUSTY_REVIEW_PROVIDER=bedrock.";

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(openrouter: bool, aws: bool) -> CredentialEnv {
        CredentialEnv { openrouter, aws }
    }

    #[test]
    fn provider_default_explicit_wins() {
        // Rule 1: an explicit provider outranks every credential signal.
        for (or, aws) in [(true, true), (true, false), (false, true), (false, false)] {
            let d = ProviderDefault::detect(Some(Provider::Bedrock), &creds(or, aws));
            assert_eq!(d.provider, Provider::Bedrock);
            assert_eq!(d.source, ProviderSource::Explicit);
        }
        let d = ProviderDefault::detect(Some(Provider::OpenRouter), &creds(false, true));
        assert_eq!(d.provider, Provider::OpenRouter);
        assert_eq!(d.source, ProviderSource::Explicit);
    }

    #[test]
    fn provider_default_openrouter_key_wins() {
        // Rule 2: a usable key selects OpenRouter even when AWS is available.
        let d = ProviderDefault::detect(None, &creds(true, true));
        assert_eq!(d.provider, Provider::OpenRouter);
        assert_eq!(d.source, ProviderSource::OpenRouterKey);
    }

    #[test]
    fn provider_default_aws_only_is_bedrock() {
        // An existing Bedrock-only setup is unchanged.
        let d = ProviderDefault::detect(None, &creds(false, true));
        assert_eq!(d.provider, Provider::Bedrock);
        assert_eq!(d.source, ProviderSource::AwsCredentials);
        assert_eq!(d.reviewer_model, crate::llm::models::DEFAULT_REVIEWER_MODEL);
        assert_eq!(d.verifier_model, crate::llm::models::DEFAULT_VERIFIER_MODEL);
        assert_eq!(
            d.summarizer_model,
            crate::llm::models::DEFAULT_SUMMARIZER_MODEL
        );
    }

    #[test]
    fn provider_default_no_credential_is_reported() {
        // Rule 4: no silent fallback — the state is reported, not papered over.
        let d = ProviderDefault::detect(None, &creds(false, false));
        assert_eq!(d.source, ProviderSource::NoCredential);
    }

    #[test]
    fn provider_default_openrouter_models_are_slugs() {
        // A Bedrock inference-profile id sent to OpenRouter is a guaranteed
        // failure; the provider default must carry OpenRouter slugs.
        let d = ProviderDefault::detect(None, &creds(true, false));
        for model in [d.reviewer_model, d.verifier_model, d.summarizer_model] {
            assert!(
                !model.starts_with("us."),
                "OpenRouter default {model} must not be a Bedrock inference-profile id"
            );
            assert!(
                model.starts_with("anthropic/"),
                "OpenRouter default {model} must be a vendor-namespaced slug"
            );
        }
    }

    #[test]
    fn provider_default_message_names_both_options() {
        assert!(NO_CREDENTIAL_MESSAGE.contains("OPENROUTER_API_KEY"));
        assert!(NO_CREDENTIAL_MESSAGE.contains("Bedrock"));
        assert!(NO_CREDENTIAL_MESSAGE.contains("TRUSTY_REVIEW_PROVIDER"));
    }

    #[test]
    fn credential_env_rejects_blank_openrouter_key() {
        // A blank or whitespace-only key must not win precedence and then fail
        // at the API boundary (#5671).
        assert!(!is_usable(None));
        for blank in ["", "   ", "\t\n"] {
            assert!(
                !is_usable(Some(blank)),
                "{blank:?} must be treated as absent"
            );
        }
        assert!(is_usable(Some("sk-or-v1-x")));

        // …and a blank key must not beat AWS: the detected env is `aws`-only.
        let d = ProviderDefault::detect(None, &creds(is_usable(Some("  ")), true));
        assert_eq!(d.provider, Provider::Bedrock);
        assert_eq!(d.source, ProviderSource::AwsCredentials);
    }
}
