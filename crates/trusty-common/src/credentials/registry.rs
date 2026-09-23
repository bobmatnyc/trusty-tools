//! The provider registry: the one `provider name → canonical env var` table
//! (issue #4564, DOC-45 `C-2.7`).
//!
//! # Spec References
//!
//! - [`SPEC-CREDAUTH-02~draft`](docs/specs/DOC-45-credential-authority-model.md#SPEC-CREDAUTH-02~draft)
//!
//! Why: `env_var_for` used to be a hand-written `match` covering 10 providers
//! while a census of production source found **23** distinct credential
//! environment-variable names in use. The 13 unregistered names were not
//! merely undocumented — they were *unroutable*: no consumer could resolve
//! them through the 3-tier chain even if it wanted to, because the env tier
//! had no name to look up. `SLACK_APP_TOKEN` was the sharpest case, sitting
//! unmapped between its two mapped siblings `SLACK_BOT_TOKEN` and
//! `SLACK_USER_TOKEN`.
//!
//! What: [`REGISTRY`] is the table; [`env_var_for`] is the case-insensitive
//! lookup over it; [`registered_providers`] exposes it for enumeration.
//! Registering a name does **not** grant anything and does not migrate any
//! consumer — DOC-45 §5 owns authorization (#4566) and #4571 owns the
//! migration of the 55 raw `std::env::var` reads. This module only makes a
//! credential *nameable*.
//!
//! Test: `tests::registry_covers_the_full_census`,
//! `tests::env_var_for_is_case_insensitive_for_every_provider`,
//! `tests::registry_has_no_duplicate_provider_keys`,
//! `tests::registry_has_no_duplicate_env_vars`.
//!
//! #8236: the table and its lookups moved to the UNCONDITIONAL
//! [`crate::credential_registry`], because the LaunchAgent plist scanner needs
//! registry membership on every feature set and this module compiles only under
//! `credentials`. The public path here is unchanged, and the census assertions
//! below stayed with the spec reference that governs them.

pub use crate::credential_registry::{
    REGISTRY, env_var_for, is_registered_credential_env_var, provider_for_env_var,
    registered_providers,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The 23 credential environment-variable names a census of production
    /// source (`crates/*/src/**`, tests excluded) found in use, recorded on
    /// `origin/main` for #4564. Listed literally so the assertion below is a
    /// statement about the workspace, not a restatement of [`REGISTRY`].
    const CENSUS: &[&str] = &[
        "FIREWORKS_API_KEY",
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "TOGETHER_API_KEY",
        "ATLASCLOUD_API_KEY",
        "SLACK_BOT_TOKEN",
        "SLACK_USER_TOKEN",
        "SLACK_APP_TOKEN",
        "TELEGRAM_BOT_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "TAGENT_API_TOKEN",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "GITHUB_APP_PRIVATE_KEY",
        "GITHUB_WEBHOOK_SECRET",
        "BITBUCKET_TOKEN",
        "BITBUCKET_APP_PASSWORD",
        "JIRA_TOKEN",
        "JIRA_API_TOKEN",
        "LINEAR_API_KEY",
        "BRAVE_API_KEY",
        "GOOGLE_OAUTH_CLIENT_SECRET",
    ];

    /// Why: the registry gap was the ticket. Pinning the census both ways —
    /// every censused name is registered, and the registry invents nothing
    /// beyond it — is what makes "complete" checkable rather than asserted.
    /// Test: itself.
    #[test]
    fn registry_covers_the_full_census() {
        let registered: Vec<&str> = REGISTRY.iter().map(|(_, var)| *var).collect();
        for name in CENSUS {
            assert!(
                registered.contains(name),
                "credential env var {name} is used in production source but unregistered — \
                 add it to credentials::registry::REGISTRY (#4564)"
            );
        }
        for var in &registered {
            assert!(
                CENSUS.contains(var),
                "registry entry {var} is not in the recorded census — update CENSUS with the \
                 new call site, or drop the entry"
            );
        }
        assert_eq!(REGISTRY.len(), CENSUS.len());
    }

    /// Why: pins the canonical mapping used everywhere else in the epic.
    /// Moved verbatim from `resolver::tests` by #4564 along with the code it
    /// covers; the enlarged set is covered by
    /// `registry_covers_the_full_census`.
    /// Test: itself.
    #[test]
    fn env_var_for_known_providers() {
        assert_eq!(env_var_for("fireworks"), Some("FIREWORKS_API_KEY"));
        assert_eq!(env_var_for("OpenRouter"), Some("OPENROUTER_API_KEY"));
        assert_eq!(env_var_for("anthropic"), Some("ANTHROPIC_API_KEY"));
        assert_eq!(env_var_for("OPENAI"), Some("OPENAI_API_KEY"));
        assert_eq!(env_var_for("together"), Some("TOGETHER_API_KEY"));
        assert_eq!(env_var_for("atlascloud"), Some("ATLASCLOUD_API_KEY"));
        // Non-inference token: the native Slack MCP server (issue #2638).
        assert_eq!(env_var_for("slack"), Some("SLACK_BOT_TOKEN"));
        assert_eq!(env_var_for("Slack"), Some("SLACK_BOT_TOKEN"));
        // Second Slack token for user-scope search methods (issue #2640).
        assert_eq!(env_var_for("slack-user"), Some("SLACK_USER_TOKEN"));
        assert_eq!(env_var_for("Slack-User"), Some("SLACK_USER_TOKEN"));
        // Non-inference token: the native Telegram MCP server (issue #2641).
        assert_eq!(env_var_for("telegram"), Some("TELEGRAM_BOT_TOKEN"));
        // trusty-agents' ctrl/PM `claude` CLI OAuth token (issue #3248).
        assert_eq!(env_var_for("claude-code"), Some("CLAUDE_CODE_OAUTH_TOKEN"));
        assert_eq!(env_var_for("Claude-Code"), Some("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    /// Why: case-insensitive lookup was the pre-existing behaviour of the
    /// `match`, relied on by callers that pass a provider id verbatim from
    /// user config. The move to a table could have silently dropped it, and
    /// the enlarged set makes spot-checking two providers insufficient.
    /// Test: itself.
    #[test]
    fn env_var_for_is_case_insensitive_for_every_provider() {
        for (provider, var) in REGISTRY {
            assert_eq!(env_var_for(provider), Some(*var), "exact: {provider}");
            assert_eq!(
                env_var_for(&provider.to_ascii_uppercase()),
                Some(*var),
                "upper: {provider}"
            );
            let mixed: String = provider
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    if i % 2 == 0 {
                        c.to_ascii_uppercase()
                    } else {
                        c
                    }
                })
                .collect();
            assert_eq!(env_var_for(&mixed), Some(*var), "mixed: {mixed}");
        }
    }

    /// Why: a duplicate provider key would make lookup order-dependent and
    /// silently shadow one of the two entries.
    /// Test: itself.
    #[test]
    fn registry_has_no_duplicate_provider_keys() {
        let mut keys: Vec<String> = REGISTRY
            .iter()
            .map(|(k, _)| k.to_ascii_lowercase())
            .collect();
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "duplicate provider key in REGISTRY");
    }

    /// Why: two provider keys mapping to the same env var means one of them
    /// cannot be independently stored or revoked — the defect `slack` /
    /// `slack-user` were deliberately split to avoid.
    /// Test: itself.
    #[test]
    fn registry_has_no_duplicate_env_vars() {
        let mut vars: Vec<&str> = REGISTRY.iter().map(|(_, v)| *v).collect();
        vars.sort_unstable();
        let before = vars.len();
        vars.dedup();
        assert_eq!(before, vars.len(), "duplicate env var in REGISTRY");
    }

    /// Why: an unregistered provider must not panic or synthesise a guess —
    /// `None` means "the env tier does not apply", and the store tier still
    /// gets its chance.
    /// Test: itself.
    #[test]
    fn env_var_for_unknown_provider_is_none() {
        assert_eq!(env_var_for("some-future-provider"), None);
        assert_eq!(env_var_for(""), None);
    }

    /// Why: the public accessor must not drift from the table it exposes.
    /// Test: itself.
    #[test]
    fn registered_providers_matches_the_table() {
        assert_eq!(registered_providers(), REGISTRY);
    }

    /// Why: #4564 promised out-of-tree consumers one release of grace. A
    /// compile-time use of the deprecated alias is the only way to prove the
    /// shim still resolves.
    /// Test: itself.
    #[test]
    #[allow(deprecated)]
    fn deprecated_inference_alias_still_resolves() {
        assert_eq!(
            crate::inference::credentials::env_var_for("slack-app"),
            Some("SLACK_APP_TOKEN")
        );
    }
}
