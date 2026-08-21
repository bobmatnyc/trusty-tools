//! Hand-written `Debug` for every config struct that holds a credential.
//!
//! Why: these config structs derived `Debug` over a plaintext credential, so any
//! `{:?}` of one — a `tracing` field, an `anyhow` context, a `dbg!`, a panic
//! message — printed the live secret (#5770). The impls live in one file rather
//! than beside each struct so a single place answers "which config fields are
//! credentials, and is every one of them masked".
//! What: each impl destructures `Self` exhaustively, renders every
//! non-credential field verbatim, and replaces the credential with
//! [`REDACTED`]. The mask is fixed: no prefix, no length, nothing derived from
//! the value.
//! Test: the `debug_never_renders_*` tests in this module's `tests`.
//!
//! # Why every impl destructures instead of reading `self.field`
//!
//! Reading fields by name and calling `.finish()` compiles fine when a field is
//! added, and that field then silently vanishes from `Debug` output. For a
//! credential that is worse than the bug this module fixes: invisible rather
//! than leaked, so the next occurrence never surfaces. `let Self { .. } = self`
//! with no `..` rest pattern makes the compiler reject any struct that grows a
//! field until this file names it, which forces a decision about whether the new
//! field is a secret. Keeping the impls in a different file from the structs is
//! what makes that compiler check load-bearing.
//!
//! [`ClassificationEngineConfig`] lives in `crate::classify` rather than under
//! `core::config`, and its impl is here anyway: it carries a clone of
//! [`ClassificationConfig`]'s OpenRouter key, so splitting the two would put half
//! the answer in another file.

use std::fmt;

use super::azdo::AzureDevOpsConfig;
use super::{BitbucketConfig, ClassificationConfig, GithubConfig, JiraConfig, LinearConfig};
use crate::classify::classifier::ClassificationEngineConfig;

/// What a credential field renders as in `Debug` output.
///
/// Unconditional, and deliberately carries nothing from the value. None of these
/// fields is format-validated — they are plain `String` / `Option<String>` filled
/// from YAML or an env var — so a fingerprint helper that echoes a fixed-length
/// head, such as [`trusty_common::credentials::redact_secret`], would disclose
/// four characters of real entropy for any credential whose shape is not the
/// provider's documented one. Settled on #5733.
const REDACTED: &str = "<redacted>";

/// Mask a credential that is always present.
///
/// Every masked field routes through here, including the optional ones via
/// [`mask`], so a change to the masking rule has exactly one site — which is
/// also what lets one mutation exercise all six impls in the tests.
fn mask_required(_value: &str) -> &'static str {
    REDACTED
}

/// Mask an optional credential while keeping whether it is set.
///
/// `None` stays `None`: absence is not a secret, and "is a credential configured
/// at all" is the first question anyone debug-formats a config to answer.
fn mask(value: Option<&String>) -> Option<&'static str> {
    value.map(|v| mask_required(v))
}

/// Redacting `Debug` — the derived one printed `api_key` verbatim (#5770).
/// Test: `debug_never_renders_the_linear_api_key`.
impl fmt::Debug for LinearConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            api_key,
            team_keys,
            fetch_on_reference,
            ticket_regex,
        } = self;
        f.debug_struct("LinearConfig")
            .field("api_key", &mask(api_key.as_ref()))
            .field("team_keys", team_keys)
            .field("fetch_on_reference", fetch_on_reference)
            .field("ticket_regex", ticket_regex)
            .finish()
    }
}

/// Redacting `Debug` — the derived one printed `token` verbatim (#5770).
/// Test: `debug_never_renders_the_github_token`.
impl fmt::Debug for GithubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            token,
            org,
            orgs,
            repo,
            fetch_prs,
            fetch_pr_reviews,
            review_fetch_concurrency,
            ticket_regex,
            fetch_on_reference,
            work_items_unavailable,
        } = self;
        f.debug_struct("GithubConfig")
            .field("token", &mask(token.as_ref()))
            .field("org", org)
            .field("orgs", orgs)
            .field("repo", repo)
            .field("fetch_prs", fetch_prs)
            .field("fetch_pr_reviews", fetch_pr_reviews)
            .field("review_fetch_concurrency", review_fetch_concurrency)
            .field("ticket_regex", ticket_regex)
            .field("fetch_on_reference", fetch_on_reference)
            .field("work_items_unavailable", work_items_unavailable)
            .finish()
    }
}

/// Redacting `Debug` — the derived one printed both `app_password` and `token`
/// verbatim (#5770). Bitbucket carries two independent secrets, one per auth
/// mode, and a config may legitimately populate both during a migration.
/// Test: `debug_never_renders_either_bitbucket_secret`.
impl fmt::Debug for BitbucketConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            username,
            app_password,
            token,
            workspace,
            repo_slug,
            fetch_prs,
            api_base_url,
        } = self;
        f.debug_struct("BitbucketConfig")
            .field("username", username)
            .field("app_password", &mask(app_password.as_ref()))
            .field("token", &mask(token.as_ref()))
            .field("workspace", workspace)
            .field("repo_slug", repo_slug)
            .field("fetch_prs", fetch_prs)
            .field("api_base_url", api_base_url)
            .finish()
    }
}

/// Redacting `Debug` — the derived one printed `token` verbatim (#5770).
/// `username` stays in the clear: for JIRA Cloud it is an email address, which is
/// the field that tells an operator which account the run authenticated as.
/// Test: `debug_never_renders_the_jira_token`.
impl fmt::Debug for JiraConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            url,
            username,
            token,
            project_key,
            timezone,
            jira_project_mappings,
            jira_project_mapping_confidence,
            ticket_regex,
            fetch_on_reference,
        } = self;
        f.debug_struct("JiraConfig")
            .field("url", url)
            .field("username", username)
            .field("token", &mask(token.as_ref()))
            .field("project_key", project_key)
            .field("timezone", timezone)
            .field("jira_project_mappings", jira_project_mappings)
            .field(
                "jira_project_mapping_confidence",
                jira_project_mapping_confidence,
            )
            .field("ticket_regex", ticket_regex)
            .field("fetch_on_reference", fetch_on_reference)
            .finish()
    }
}

/// Redacting `Debug` — the derived one printed `pat` verbatim (#5770).
///
/// `pat` is a bare `String`, not an `Option`, so there is no unset state to
/// distinguish: the mask is printed for every value including the empty one,
/// which config validation rejects but `Debug` must still handle.
/// Test: `debug_never_renders_the_azdo_pat`.
impl fmt::Debug for AzureDevOpsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            organization_url,
            pat,
            project,
            projects,
            ticket_regex,
            team_keys,
            fetch_on_reference,
            fetch_prs,
        } = self;
        f.debug_struct("AzureDevOpsConfig")
            .field("organization_url", organization_url)
            .field("pat", &mask_required(pat))
            .field("project", project)
            .field("projects", projects)
            .field("ticket_regex", ticket_regex)
            .field("team_keys", team_keys)
            .field("fetch_on_reference", fetch_on_reference)
            .field("fetch_prs", fetch_prs)
            .finish()
    }
}

/// Redacting `Debug` — the derived one printed `openrouter_api_key` verbatim
/// (#5770).
///
/// [`super::Config`] embeds this section and derives `Debug`, so before this impl
/// a `{:?}` of a loaded config printed the OpenRouter key. `llm_provider` and
/// `llm_model` stay in the clear — they are the fields that explain which
/// provider a classification run actually selected.
/// Test: `debug_never_renders_the_openrouter_key_in_the_config_section`.
impl fmt::Debug for ClassificationConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            rules_files,
            repo_categories,
            use_llm,
            llm_model,
            llm_provider,
            openrouter_api_key,
            confidence_threshold,
            custom_categories,
            min_coverage_pct,
            llm_fallback_threshold,
            weighted_sum,
            llm_fallback_concurrency,
            no_external,
            checkpoint_every,
            sources,
        } = self;
        f.debug_struct("ClassificationConfig")
            .field("rules_files", rules_files)
            .field("repo_categories", repo_categories)
            .field("use_llm", use_llm)
            .field("llm_model", llm_model)
            .field("llm_provider", llm_provider)
            .field("openrouter_api_key", &mask(openrouter_api_key.as_ref()))
            .field("confidence_threshold", confidence_threshold)
            .field("custom_categories", custom_categories)
            .field("min_coverage_pct", min_coverage_pct)
            .field("llm_fallback_threshold", llm_fallback_threshold)
            .field("weighted_sum", weighted_sum)
            .field("llm_fallback_concurrency", llm_fallback_concurrency)
            .field("no_external", no_external)
            .field("checkpoint_every", checkpoint_every)
            .field("sources", sources)
            .finish()
    }
}

/// Redacting `Debug` — the derived one printed `openrouter_api_key` verbatim
/// (#5770).
///
/// `ClassificationPipeline` clones the key out of [`ClassificationConfig`] into
/// this struct, so fixing only the config would leave the same credential
/// readable one struct downstream.
/// Test: `debug_never_renders_the_openrouter_key_in_the_engine_config`.
impl fmt::Debug for ClassificationEngineConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            use_llm,
            llm_model,
            llm_provider,
            openrouter_api_key,
            confidence_threshold,
            weighted_sum,
        } = self;
        f.debug_struct("ClassificationEngineConfig")
            .field("use_llm", use_llm)
            .field("llm_model", llm_model)
            .field("llm_provider", llm_provider)
            .field("openrouter_api_key", &mask(openrouter_api_key.as_ref()))
            .field("confidence_threshold", confidence_threshold)
            .field("weighted_sum", weighted_sum)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Credential shapes worth covering, with why each is here.
    ///
    /// Nothing validates these fields' format, so the table must not assume the
    /// provider's documented prefix: a mask that echoed a fixed-length head would
    /// look safe against a `ghp_`-prefixed token and disclose real entropy
    /// against everything else.
    ///
    /// No single-character case: the rendered output contains the mask and the
    /// probe fields, so a one-letter needle trips `contains` against those and
    /// fails a correct mask. Two characters is the shortest honest case.
    const SECRET_SHAPES: &[(&str, &str)] = &[
        (
            "9f3Kq7Zt2Wm4Bx8Lv6Nc1Rd5Ph0Sj",
            "no prefix: entropy up front",
        ),
        (
            "ghp_averyrealisticlookingtoken0123456789",
            "a provider-prefixed token",
        ),
        ("ab7Q", "exactly a four-character head"),
        ("x9", "shorter than a head"),
        ("", "empty — rejected by validation, guarded anyway"),
    ];

    /// A non-secret field value asserted to survive redaction. Distinct from
    /// every entry in [`SECRET_SHAPES`] so a "the secret leaked" assertion can
    /// never be satisfied by this instead.
    const PROBE: &str = "probe-value-kept-in-the-clear";

    /// Assert `rendered` masked `secret` and kept [`PROBE`].
    ///
    /// The leading-fragment check is the point: asserting only that the whole
    /// secret is absent passes for a head-echoing redactor, which is the exact
    /// disclosure #5733 ruled out.
    fn assert_masked(rendered: &str, secret: &str, why: &str) {
        if !secret.is_empty() {
            assert!(
                !rendered.contains(secret),
                "{why}: the whole credential reached Debug output: {rendered}"
            );
            let head: String = secret.chars().take(4).collect();
            assert!(
                !rendered.contains(&head),
                "{why}: a leading fragment of the credential survived: {rendered}"
            );
        }
        assert!(
            rendered.contains(REDACTED),
            "{why}: the credential field was not masked: {rendered}"
        );
        assert!(
            rendered.contains(PROBE),
            "{why}: redaction must not cost the non-secret fields: {rendered}"
        );
    }

    /// Run `render` over every shape in [`SECRET_SHAPES`], compact and pretty.
    fn for_each_shape(render: impl Fn(&str) -> String) {
        for (secret, why) in SECRET_SHAPES {
            assert_masked(&render(secret), secret, why);
        }
    }

    #[test]
    fn debug_never_renders_the_linear_api_key() {
        for_each_shape(|secret| {
            let cfg = LinearConfig {
                api_key: Some(secret.to_string()),
                ticket_regex: Some(PROBE.to_string()),
                ..LinearConfig::default()
            };
            format!("{cfg:?}{cfg:#?}")
        });

        let unset = LinearConfig {
            api_key: None,
            ticket_regex: Some(PROBE.to_string()),
            ..LinearConfig::default()
        };
        let rendered = format!("{unset:?}");
        assert!(
            rendered.contains("api_key: None"),
            "an unset key must stay visibly unset — that is what the reader came \
             for: {rendered}"
        );
    }

    #[test]
    fn debug_never_renders_the_github_token() {
        for_each_shape(|secret| {
            let cfg = GithubConfig {
                token: Some(secret.to_string()),
                org: Some(PROBE.to_string()),
                ..GithubConfig::default()
            };
            format!("{cfg:?}{cfg:#?}")
        });

        let unset = GithubConfig {
            token: None,
            org: Some(PROBE.to_string()),
            ..GithubConfig::default()
        };
        let rendered = format!("{unset:?}");
        assert!(
            rendered.contains("token: None"),
            "an unset token must stay visibly unset: {rendered}"
        );
    }

    #[test]
    fn debug_never_renders_the_jira_token() {
        for_each_shape(|secret| {
            let cfg = JiraConfig {
                token: Some(secret.to_string()),
                url: Some(PROBE.to_string()),
                ..JiraConfig::default()
            };
            format!("{cfg:?}{cfg:#?}")
        });

        let unset = JiraConfig {
            token: None,
            url: Some(PROBE.to_string()),
            ..JiraConfig::default()
        };
        let rendered = format!("{unset:?}");
        assert!(
            rendered.contains("token: None"),
            "an unset token must stay visibly unset: {rendered}"
        );
    }

    /// Bitbucket carries two independent secrets — Basic-auth `app_password` and
    /// Bearer `token` — and a migrating config populates both. Both are set to
    /// the same shape here so a mask applied to only one fails.
    #[test]
    fn debug_never_renders_either_bitbucket_secret() {
        for_each_shape(|secret| {
            let cfg = BitbucketConfig {
                app_password: Some(secret.to_string()),
                token: Some(secret.to_string()),
                workspace: Some(PROBE.to_string()),
                ..BitbucketConfig::default()
            };
            format!("{cfg:?}{cfg:#?}")
        });

        // One field at a time: a mask that reads the wrong field would pass the
        // both-set case above by masking whichever it does read.
        for (secret, why) in SECRET_SHAPES {
            let password_only = BitbucketConfig {
                app_password: Some(secret.to_string()),
                token: None,
                workspace: Some(PROBE.to_string()),
                ..BitbucketConfig::default()
            };
            assert_masked(&format!("{password_only:?}"), secret, why);

            let token_only = BitbucketConfig {
                app_password: None,
                token: Some(secret.to_string()),
                workspace: Some(PROBE.to_string()),
                ..BitbucketConfig::default()
            };
            assert_masked(&format!("{token_only:?}"), secret, why);
        }

        let unset = BitbucketConfig {
            workspace: Some(PROBE.to_string()),
            ..BitbucketConfig::default()
        };
        let rendered = format!("{unset:?}");
        assert!(
            rendered.contains("app_password: None") && rendered.contains("token: None"),
            "unset secrets must stay visibly unset: {rendered}"
        );
    }

    /// `pat` is a bare `String`: empty is its only degenerate case, and there is
    /// no unset state to keep visible.
    #[test]
    fn debug_never_renders_the_azdo_pat() {
        for_each_shape(|secret| {
            let cfg = AzureDevOpsConfig {
                organization_url: PROBE.to_string(),
                pat: secret.to_string(),
                project: None,
                projects: Vec::new(),
                ticket_regex: r"(?i)\bAB#(\d+)\b".to_string(),
                team_keys: Vec::new(),
                fetch_on_reference: true,
                fetch_prs: false,
            };
            format!("{cfg:?}{cfg:#?}")
        });
    }

    /// The OpenRouter key is the one in this set that is actively threaded
    /// through a live path — `validator.rs` reads it and `classify::pipeline`
    /// clones it — and [`super::super::Config`] embeds this section while
    /// deriving `Debug`, so a `{:?}` of a loaded config printed it.
    #[test]
    fn debug_never_renders_the_openrouter_key_in_the_config_section() {
        for_each_shape(|secret| {
            let cfg = ClassificationConfig {
                openrouter_api_key: Some(secret.to_string()),
                llm_provider: PROBE.to_string(),
                ..ClassificationConfig::default()
            };
            format!("{cfg:?}{cfg:#?}")
        });

        let unset = ClassificationConfig {
            openrouter_api_key: None,
            llm_provider: PROBE.to_string(),
            ..ClassificationConfig::default()
        };
        let rendered = format!("{unset:?}");
        assert!(
            rendered.contains("openrouter_api_key: None"),
            "an unset key must stay visibly unset: {rendered}"
        );
    }

    /// The downstream clone of the same key. Fixing only the config section
    /// would leave the credential readable one struct further along the
    /// classification path.
    #[test]
    fn debug_never_renders_the_openrouter_key_in_the_engine_config() {
        for_each_shape(|secret| {
            let cfg = ClassificationEngineConfig {
                openrouter_api_key: Some(secret.to_string()),
                llm_provider: PROBE.to_string(),
                ..ClassificationEngineConfig::default()
            };
            format!("{cfg:?}{cfg:#?}")
        });

        let unset = ClassificationEngineConfig {
            openrouter_api_key: None,
            llm_provider: PROBE.to_string(),
            ..ClassificationEngineConfig::default()
        };
        let rendered = format!("{unset:?}");
        assert!(
            rendered.contains("openrouter_api_key: None"),
            "an unset key must stay visibly unset: {rendered}"
        );
    }
}
