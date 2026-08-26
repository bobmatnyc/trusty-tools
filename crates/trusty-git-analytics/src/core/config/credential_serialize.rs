//! Hand-written `Serialize` for every config struct that holds a credential.
//!
//! Why: the same six credential fields [`super::credential_debug`] masks in
//! `Debug` were also written in the clear by a **derived** `Serialize` (#5775).
//! A `Debug` leak reaches a log; a `Serialize` leak reaches a file, a manifest,
//! or a response body, so it is the worse of the pair even though nothing in
//! this crate serializes a config today.
//! What: each impl destructures `Self` exhaustively, writes every non-credential
//! field verbatim, and writes the `<redacted>` marker in place of the
//! credential — through the same constant and the same two helpers the `Debug`
//! impls use, so the masking rule has exactly one definition. [`super::Config`]
//! and [`super::PmConfig`] embed these sections and still derive `Serialize`, so
//! they inherit the redaction.
//! Test: the `serialize_never_renders_*` tests in
//! `crates/trusty-git-analytics/tests/config_serialize_redaction.rs`, which live
//! outside the crate so they compile unchanged against the pre-fix source.
//!
//! # The strategy, and what it costs (#5775 closure condition 1)
//!
//! Three strategies were on the table: a redacting `Serialize`, field-level
//! `#[serde(skip_serializing)]` plus a documented reload path, or dropping
//! `Serialize` from these types outright. This module is the first.
//!
//! The consequence is a deliberate one-way trip: serializing a config and
//! reading it back yields the literal string `<redacted>` where a credential
//! was, not the credential. No call site in this workspace writes a config out,
//! so nothing round-trips today; if one ever does, the marker is not a valid
//! token for any of the five providers, so the reload fails at authentication
//! rather than silently authenticating as someone else. That is the accepted
//! break, and it is the reason this file exists rather than a
//! `skip_serializing` attribute — a skipped field deserializes back as `None`,
//! which reads as "no credential configured" and is indistinguishable from a
//! config that never had one.
//!
//! # Why not a secret newtype
//!
//! A wrapper type that redacts by construction is the stronger shape in general,
//! and the workspace already has three of them —
//! [`trusty_common::credentials::Secret`],
//! `trusty_common::inference::types::SecretString`, and
//! `trusty_audit::config::SecretKey`. None can be used here: all three refuse
//! `Serialize` on purpose (DOC-45 `C-8.3`, and `Secret` pins the refusal with a
//! compile-time coherence assertion), so a config field of that type would strip
//! `Serialize` from [`super::Config`] entirely rather than redact it. A fourth
//! wrapper, one that serializes to a marker, would change six public field types
//! and every authentication call site in `collect::` — a breaking change to a
//! published crate, spent on a leak that is real but latent.
//!
//! What that wrapper would have bought is the guarantee that a *newly added*
//! credential field is redacted without anyone remembering. The exhaustive
//! destructure buys the same guarantee for these six structs by a different
//! route: a field added to any of them fails to compile (`E0027`) until this
//! file names it, and a field named but not written trips `unused_variables`
//! under `-D warnings`. Both checks are the compiler's, not a reviewer's.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use super::azdo::AzureDevOpsConfig;
use super::credential_debug::{mask, mask_required};
use super::{BitbucketConfig, ClassificationConfig, GithubConfig, JiraConfig, LinearConfig};

/// Redacting `Serialize` — the derived one wrote `api_key` in the clear (#5775).
/// Test: `serialize_never_renders_the_linear_api_key`.
impl Serialize for LinearConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Self {
            api_key,
            team_keys,
            fetch_on_reference,
            ticket_regex,
        } = self;
        let mut s = serializer.serialize_struct("LinearConfig", 4)?;
        s.serialize_field("api_key", &mask(api_key.as_ref()))?;
        s.serialize_field("team_keys", team_keys)?;
        s.serialize_field("fetch_on_reference", fetch_on_reference)?;
        s.serialize_field("ticket_regex", ticket_regex)?;
        s.end()
    }
}

/// Redacting `Serialize` — the derived one wrote `token` in the clear (#5775).
/// Test: `serialize_never_renders_the_github_token`.
impl Serialize for GithubConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
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
        let mut s = serializer.serialize_struct("GithubConfig", 10)?;
        s.serialize_field("token", &mask(token.as_ref()))?;
        s.serialize_field("org", org)?;
        s.serialize_field("orgs", orgs)?;
        s.serialize_field("repo", repo)?;
        s.serialize_field("fetch_prs", fetch_prs)?;
        s.serialize_field("fetch_pr_reviews", fetch_pr_reviews)?;
        s.serialize_field("review_fetch_concurrency", review_fetch_concurrency)?;
        s.serialize_field("ticket_regex", ticket_regex)?;
        s.serialize_field("fetch_on_reference", fetch_on_reference)?;
        s.serialize_field("work_items_unavailable", work_items_unavailable)?;
        s.end()
    }
}

/// Redacting `Serialize` — the derived one wrote both `app_password` and `token`
/// in the clear (#5775). Bitbucket carries one secret per auth mode and a
/// migrating config may populate both.
/// Test: `serialize_never_renders_either_bitbucket_secret`.
impl Serialize for BitbucketConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Self {
            username,
            app_password,
            token,
            workspace,
            repo_slug,
            fetch_prs,
            api_base_url,
        } = self;
        let mut s = serializer.serialize_struct("BitbucketConfig", 7)?;
        s.serialize_field("username", username)?;
        s.serialize_field("app_password", &mask(app_password.as_ref()))?;
        s.serialize_field("token", &mask(token.as_ref()))?;
        s.serialize_field("workspace", workspace)?;
        s.serialize_field("repo_slug", repo_slug)?;
        s.serialize_field("fetch_prs", fetch_prs)?;
        s.serialize_field("api_base_url", api_base_url)?;
        s.end()
    }
}

/// Redacting `Serialize` — the derived one wrote `token` in the clear (#5775).
/// `username` stays in the clear for the same reason it does in `Debug`: for
/// JIRA Cloud it is an email address, and it is what names the account.
/// Test: `serialize_never_renders_the_jira_token`.
impl Serialize for JiraConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
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
        let mut s = serializer.serialize_struct("JiraConfig", 9)?;
        s.serialize_field("url", url)?;
        s.serialize_field("username", username)?;
        s.serialize_field("token", &mask(token.as_ref()))?;
        s.serialize_field("project_key", project_key)?;
        s.serialize_field("timezone", timezone)?;
        s.serialize_field("jira_project_mappings", jira_project_mappings)?;
        s.serialize_field(
            "jira_project_mapping_confidence",
            jira_project_mapping_confidence,
        )?;
        s.serialize_field("ticket_regex", ticket_regex)?;
        s.serialize_field("fetch_on_reference", fetch_on_reference)?;
        s.end()
    }
}

/// Redacting `Serialize` — the derived one wrote `pat` in the clear (#5775).
///
/// `pat` is a bare `String`, so there is no unset state to preserve: the marker
/// is written for every value including the empty one, which config validation
/// rejects but serialization must still handle.
/// Test: `serialize_never_renders_the_azdo_pat`.
impl Serialize for AzureDevOpsConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
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
        let mut s = serializer.serialize_struct("AzureDevOpsConfig", 8)?;
        s.serialize_field("organization_url", organization_url)?;
        s.serialize_field("pat", mask_required(pat))?;
        s.serialize_field("project", project)?;
        s.serialize_field("projects", projects)?;
        s.serialize_field("ticket_regex", ticket_regex)?;
        s.serialize_field("team_keys", team_keys)?;
        s.serialize_field("fetch_on_reference", fetch_on_reference)?;
        s.serialize_field("fetch_prs", fetch_prs)?;
        s.end()
    }
}

/// Redacting `Serialize` — the derived one wrote `openrouter_api_key` in the
/// clear (#5775).
///
/// Not one of the five PM sections #5775 names, and included anyway: it derives
/// `Serialize` over a credential exactly as they do, [`super::Config`] embeds it
/// and derives `Serialize` too, and it is the one key in the set on a live code
/// path (`validator.rs` reads it, `classify::pipeline` clones it). Fixing five
/// of six leaks in one crate is not a fix.
/// Test: `serialize_never_renders_the_openrouter_key`.
impl Serialize for ClassificationConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
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
        let mut s = serializer.serialize_struct("ClassificationConfig", 15)?;
        s.serialize_field("rules_files", rules_files)?;
        s.serialize_field("repo_categories", repo_categories)?;
        s.serialize_field("use_llm", use_llm)?;
        s.serialize_field("llm_model", llm_model)?;
        s.serialize_field("llm_provider", llm_provider)?;
        s.serialize_field("openrouter_api_key", &mask(openrouter_api_key.as_ref()))?;
        s.serialize_field("confidence_threshold", confidence_threshold)?;
        s.serialize_field("custom_categories", custom_categories)?;
        s.serialize_field("min_coverage_pct", min_coverage_pct)?;
        s.serialize_field("llm_fallback_threshold", llm_fallback_threshold)?;
        s.serialize_field("weighted_sum", weighted_sum)?;
        s.serialize_field("llm_fallback_concurrency", llm_fallback_concurrency)?;
        s.serialize_field("no_external", no_external)?;
        s.serialize_field("checkpoint_every", checkpoint_every)?;
        s.serialize_field("sources", sources)?;
        s.end()
    }
}
