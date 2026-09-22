//! The unconditional `provider ↔ canonical env var` table (#4564, moved #8236).
//!
//! Why: the table is pure data with no dependency of its own, but it lived
//! inside the `credentials`-feature-gated module tree. #8236 needs it from
//! [`crate::launchd_secrets`], which is deliberately ungated — "a guard that
//! compiles out under some feature set is not a guard". Hoisting the data here
//! lets the plist scanner name a credential by REGISTRY membership rather than
//! by a suffix guess, on every feature set.
//!
//! What: [`REGISTRY`] is the table, [`env_var_for`] the provider → variable
//! lookup, [`provider_for_env_var`] the reverse (what #8236's `--fix` needs to
//! know WHERE to migrate a plist entry to), and
//! [`is_registered_credential_env_var`] the membership predicate the plist
//! scanner asks first. `credentials::registry` re-exports all four, so the
//! documented import path is unchanged.
//!
//! Test: `crate::credentials::registry::tests` (the census assertions stayed
//! with the module that owns the spec reference), plus
//! `provider_for_env_var_round_trips` and
//! `is_registered_credential_env_var_is_case_insensitive` here.
//!
//! [`REGISTRY`]: crate::credential_registry::REGISTRY
//! [`env_var_for`]: crate::credential_registry::env_var_for
//! [`provider_for_env_var`]: crate::credential_registry::provider_for_env_var
//! [`is_registered_credential_env_var`]: crate::credential_registry::is_registered_credential_env_var

/// Every credential this workspace knows how to name, as
/// `(provider key, canonical environment-variable name)`.
///
/// Why: a table rather than a `match` arm so a test can enumerate it. The
/// acceptance criterion for #4564 is that the registry is *checkable* — an
/// opaque `match` cannot be asserted complete, and completeness is the whole
/// point of the ticket.
/// What: provider keys are lowercase-kebab and are the identifier a caller
/// passes to [`env_var_for`] / `resolve_key`; lookup is case-insensitive
/// (see [`env_var_for`]). Two keys may name the same provider where the
/// provider genuinely has two distinct secrets (`slack` / `slack-user` /
/// `slack-app`, `github` / `github-app` / `github-webhook`). Entries are
/// grouped by origin and each non-inference group cites the ticket that
/// introduced it.
/// Test: `crate::credentials::registry::tests::registry_covers_the_full_census`.
///
/// DOC-45 `C-2.7`: a `CredentialRef` (#4565) resolves *through* this table, so
/// a provider absent from it fails with `Missing` rather than silently.
pub const REGISTRY: &[(&str, &str)] = &[
    // ── Inference providers (epic #2400, issue #2401) ──
    ("fireworks", "FIREWORKS_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("together", "TOGETHER_API_KEY"),
    ("atlascloud", "ATLASCLOUD_API_KEY"),
    // ── Chat channels ──
    // Slack bot token for the native Slack MCP server (issue #2638).
    ("slack", "SLACK_BOT_TOKEN"),
    // `search.messages` (and other user-scope-only methods) require a Slack
    // *user* token, which a bot token cannot substitute for (issue #2640).
    ("slack-user", "SLACK_USER_TOKEN"),
    // #4564: Socket Mode's app-level token — the third distinct Slack secret,
    // and the one that was unmapped between two mapped siblings.
    ("slack-app", "SLACK_APP_TOKEN"),
    ("telegram", "TELEGRAM_BOT_TOKEN"),
    // ── Agent runtime ──
    // trusty-agents' ctrl/PM OAuth routing (issue #3248): the `claude` CLI
    // subprocess token from `claude setup-token`.
    ("claude-code", "CLAUDE_CODE_OAUTH_TOKEN"),
    // #4564: the trusty-agents HTTP API bearer.
    ("tagent", "TAGENT_API_TOKEN"),
    // ── Forges (#4564) ──
    // `github` and `github-gh-cli` are deliberately separate keys for the two
    // names the same PAT is read under: `gh` reads `GH_TOKEN` in preference to
    // `GITHUB_TOKEN`, and collapsing them would make the resolver unable to
    // express which of the two a given call site actually consults.
    ("github", "GITHUB_TOKEN"),
    ("github-gh-cli", "GH_TOKEN"),
    ("github-app", "GITHUB_APP_PRIVATE_KEY"),
    ("github-webhook", "GITHUB_WEBHOOK_SECRET"),
    ("bitbucket", "BITBUCKET_TOKEN"),
    ("bitbucket-app-password", "BITBUCKET_APP_PASSWORD"),
    // ── Trackers (#4564; unblocks #4478 question (b)) ──
    ("jira", "JIRA_TOKEN"),
    ("jira-api", "JIRA_API_TOKEN"),
    ("linear", "LINEAR_API_KEY"),
    // ── Other services (#4564) ──
    ("brave", "BRAVE_API_KEY"),
    ("google-oauth", "GOOGLE_OAUTH_CLIENT_SECRET"),
];

/// Canonical process-env variable name for a provider's credential.
///
/// Why: every call site (the resolver's env tier, the `config` clap module's
/// `--env` hint, and — from #4565 — `CredentialRef` resolution) must agree on
/// one name per provider rather than re-deriving `{PROVIDER}_API_KEY` ad hoc,
/// which breaks for every provider whose canonical variable does not follow
/// that shape (`SLACK_BOT_TOKEN`, `GH_TOKEN`, `GITHUB_APP_PRIVATE_KEY`, …).
/// What: case-insensitive lookup over [`REGISTRY`]. `None` for an unregistered
/// provider; callers treat that as "the env tier does not apply", not an error.
/// Test: `crate::credentials::registry::tests::registry_covers_the_full_census`,
/// `crate::credentials::registry::tests::env_var_for_is_case_insensitive_for_every_provider`.
#[must_use]
pub fn env_var_for(provider: &str) -> Option<&'static str> {
    REGISTRY
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(provider))
        .map(|(_, var)| *var)
}

/// The provider key a canonical environment-variable name belongs to.
///
/// Why: #8236's `tm doctor --fix` reads a credential KEY out of a LaunchAgent
/// plist and has to decide where that value belongs in the credential store.
/// Stripping the plist entry without an import target would remove a working
/// configuration and disable the feature, so a key with no answer here is
/// REPORTED and left alone rather than deleted.
/// What: the reverse of [`env_var_for`], case-insensitive on the variable name.
/// `None` when the variable is not registered.
/// Test: `provider_for_env_var_round_trips`,
/// `provider_for_env_var_is_none_for_an_unregistered_name`.
#[must_use]
pub fn provider_for_env_var(var: &str) -> Option<&'static str> {
    REGISTRY
        .iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(var.trim()))
        .map(|(key, _)| *key)
}

/// Is `var` a registered credential environment variable?
///
/// Why: the registry-first half of #8236's plist detection. A name in this
/// table is a credential by declaration, so it never depends on a suffix
/// heuristic agreeing — which is what let `AWS_PROFILE`-shaped keys and
/// `SLACK_APP_TOKEN` disagree with each other before.
/// Test: `is_registered_credential_env_var_is_case_insensitive`.
#[must_use]
pub fn is_registered_credential_env_var(var: &str) -> bool {
    provider_for_env_var(var).is_some()
}

/// Every registered `(provider key, env var)` pair.
///
/// Why: #4565's `CredentialRef` grammar and the `config keys list` surface both
/// need to enumerate what is nameable; reaching into [`REGISTRY`] directly
/// would leak the table's layout into consumers.
/// Test: `crate::credentials::registry::tests::registered_providers_matches_the_table`.
#[must_use]
pub fn registered_providers() -> &'static [(&'static str, &'static str)] {
    REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `--fix` picks its import target with this lookup; a reverse map
    /// that disagreed with the forward one would store a value under a
    /// provider nothing ever reads.
    #[test]
    fn provider_for_env_var_round_trips() {
        for (provider, var) in REGISTRY {
            assert_eq!(provider_for_env_var(var), Some(*provider), "{var}");
            assert_eq!(env_var_for(provider), Some(*var), "{provider}");
        }
    }

    /// Why: an unregistered credential-shaped key is the case that must NOT be
    /// stripped from a plist — see [`provider_for_env_var`].
    #[test]
    fn provider_for_env_var_is_none_for_an_unregistered_name() {
        assert_eq!(provider_for_env_var("AWS_SECRET_ACCESS_KEY"), None);
        assert_eq!(provider_for_env_var(""), None);
    }

    /// Why: plist keys arrive verbatim from a hand-edited file, so case and
    /// stray whitespace are both realistic.
    #[test]
    fn is_registered_credential_env_var_is_case_insensitive() {
        assert!(is_registered_credential_env_var("OPENROUTER_API_KEY"));
        assert!(is_registered_credential_env_var(" telegram_bot_token "));
        assert!(!is_registered_credential_env_var("PATH"));
    }
}
