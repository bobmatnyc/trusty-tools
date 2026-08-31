//! Operator-configured default inference provider for unpinned agents (#3766).
//!
//! Why: every bundled agent template ships a BARE model slug
//! (`model = "claude-sonnet-4-6"`, seven templates as of this change) and no
//! `[agent].provider_id`. Which provider actually serves such a template was
//! therefore decided by whichever ambient credential
//! `llm::credentials::pick_credentials` happened to find, so the same template
//! ran on Anthropic-direct on one machine and OpenRouter on the next, and the
//! provider changed under a template when an unrelated environment variable
//! appeared. The owner's ruling on #3766 is that this belongs in
//! CONFIGURATION — one operator-settable policy the templates defer to — not
//! hardcoded per template, which would have to be re-decided in seven files
//! and re-applied after every reprovision.
//! What: the `[providers] default_provider_id` key of the crate's existing
//! operator config (`~/.trusty-agents/config.toml`). UNSET is the default and
//! means "keep today's ambient behaviour, byte for byte". When set, it is
//! adopted as the `provider_id` of every agent that declares neither a pin nor
//! a provider in its model slug ([`applies_to`]), which routes it through the
//! #3765 pin machinery — validation, slug rewrite, and the ambient-routing
//! skip in `ctrl::config::apply_credential_routing` — rather than a second,
//! parallel mechanism. The policy lives OUTSIDE the template files, so a
//! bundled-template reprovision (`agents::bundled`) cannot regress it.
//! Test: the `tests` module below, plus `agents::tests::provider_policy` for
//! the end-to-end loader behaviour.

use std::path::Path;

use trusty_common::inference::registry::ProviderId;

/// The `~/.trusty-agents/config.toml` table this policy is read from.
pub const CONFIG_TABLE: &str = "providers";

/// The key inside [`CONFIG_TABLE`] naming the default provider.
pub const CONFIG_KEY: &str = "default_provider_id";

/// Whether the operator's default-provider policy governs this agent.
///
/// Why: the policy is a DEFAULT, so it must lose to anything more specific.
/// Two things are more specific: an explicit `[agent].provider_id` (the
/// operator already answered this question for that agent, #3765) and a model
/// slug that names its own provider (`bedrock/…`, `openai/…`, `ollama/…` —
/// the provider-named templates, which exist precisely to pin a provider).
/// Applying the policy to either would silently overrule a statement the
/// config already makes, which is the same defect #3766 is fixing in the other
/// direction.
/// What: `true` only when `declared_provider_id` is absent or blank AND
/// `model` carries no provider prefix [`ProviderId::from_slug_prefix`]
/// recognises. Blank is treated as absent because that is what clearing the
/// field in the GUI leaves behind (the reading
/// [`crate::llm::provider_pin::resolve`] already uses).
/// Test: `policy_governs_a_bare_slug_with_no_pin`,
/// `policy_defers_to_an_explicit_pin`,
/// `policy_defers_to_a_provider_named_slug`.
pub fn applies_to(declared_provider_id: Option<&str>, model: &str) -> bool {
    declared_provider_id
        .map(str::trim)
        .is_none_or(str::is_empty)
        && ProviderId::from_slug_prefix(model).is_none()
}

/// Extract `[providers] default_provider_id` from an operator config body.
///
/// Why: the parse is separated from the file read so the policy's meaning can
/// be tested without touching `$HOME` — a process-global mutation that would
/// leak a policy into every other test loading an agent concurrently.
/// What: returns the trimmed value, or `None` for an absent table, an absent
/// key, a blank value, or a body that does not parse. Failing SOFT here is
/// deliberate and is not a hole: an unreadable config yields "no policy", i.e.
/// today's ambient behaviour, whereas a policy that IS read but names an
/// unusable provider still fails closed at
/// [`crate::llm::provider_pin::resolve`]. Unknown keys are ignored so this
/// composes with the rest of `config.toml`, matching every other section
/// loader in this crate.
/// Test: `parse_reads_the_configured_provider`, `parse_ignores_other_tables`,
/// `parse_treats_a_blank_value_as_unset`, `parse_tolerates_a_malformed_body`.
pub fn parse(config_toml: &str) -> Option<String> {
    #[derive(serde::Deserialize, Default)]
    struct Providers {
        #[serde(default)]
        default_provider_id: Option<String>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Wrapper {
        #[serde(default)]
        providers: Providers,
    }

    toml::from_str::<Wrapper>(config_toml)
        .ok()?
        .providers
        .default_provider_id
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Read the policy from `<home>/.trusty-agents/config.toml`.
///
/// Why: takes `home` as an argument rather than resolving it, so a test can
/// point it at a tempdir without redirecting the process-global `$HOME` — the
/// mutation `tests/home_lock_discipline.rs` exists to keep under one lock.
/// What: reads the file and delegates to [`parse`]; a missing or unreadable
/// file is `None`.
/// Test: `read_from_home_finds_the_configured_provider`,
/// `read_from_home_is_none_without_a_config_file`.
pub fn read_from_home(home: &Path) -> Option<String> {
    let path = home.join(".trusty-agents").join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    parse(&text)
}

/// The policy in effect for this process, or `None` when unset.
///
/// Why: the single ambient entry point the agent loader calls. Kept a thin
/// `$HOME`-resolving wrapper over [`read_from_home`] — the same shape
/// `agents::bundled::ensure_bundled_agents_deployed` uses — so all the
/// behaviour is in the hermetically testable half.
/// Test: [`read_from_home`]'s tests cover the body; this wrapper is exercised
/// by real `tagent` invocations.
pub fn configured_default() -> Option<String> {
    read_from_home(&dirs::home_dir()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the case the policy exists for — a bundled template with a bare
    /// `claude-*` slug and no pin.
    /// Test: itself.
    #[test]
    fn policy_governs_a_bare_slug_with_no_pin() {
        assert!(applies_to(None, "claude-sonnet-4-6"));
        assert!(applies_to(Some(""), "claude-sonnet-4-6"));
        assert!(applies_to(Some("  "), "claude-sonnet-4-6"));
    }

    /// Why: #3765's explicit per-agent pin is more specific than a default and
    /// must win; the policy filling it in would silently retarget the agent.
    /// Test: itself.
    #[test]
    fn policy_defers_to_an_explicit_pin() {
        assert!(!applies_to(Some("atlascloud"), "claude-sonnet-4-6"));
    }

    /// Why: the provider-named templates (`bedrock-engineer`,
    /// `gpt5-codex-engineer`, `gpt-engineer`) declare their provider IN the
    /// slug. A policy that overwrote those would break the exact templates
    /// whose whole purpose is to name a provider.
    /// Test: itself.
    #[test]
    fn policy_defers_to_a_provider_named_slug() {
        for model in [
            "bedrock/us.anthropic.claude-3-5-haiku-20241022-v1:0",
            "openai/gpt-5.1-codex",
            "anthropic/claude-sonnet-4-6",
            "ollama/qwen3:8b",
            "atlascloud/openai/gpt-5.6-sol",
        ] {
            assert!(!applies_to(None, model), "{model}");
        }
    }

    /// Why: the key is the operator's whole interface to this feature.
    /// Test: itself.
    #[test]
    fn parse_reads_the_configured_provider() {
        let body = format!("[{CONFIG_TABLE}]\n{CONFIG_KEY} = \"bedrock\"\n");
        assert_eq!(parse(&body).as_deref(), Some("bedrock"));
        assert_eq!(
            parse("[providers]\ndefault_provider_id = \"  anthropic  \"\n").as_deref(),
            Some("anthropic")
        );
    }

    /// Why: `config.toml` is shared with `[mcp]`, `[okg]`, `[search]` and the
    /// tool registry; reading it must not depend on what else is in it.
    /// Test: itself.
    #[test]
    fn parse_ignores_other_tables() {
        let body = "[mcp]\ninject_for_roles = [\"ctrl\"]\n\n\
                    [providers]\ndefault_provider_id = \"bedrock\"\n\n\
                    [search]\ncool_after_minutes = 15\n";
        assert_eq!(parse(body).as_deref(), Some("bedrock"));
        assert_eq!(parse("[mcp]\ninject_for_roles = []\n"), None);
        assert_eq!(parse(""), None);
    }

    /// Why: clearing the field in an editor leaves `""`, which must read as
    /// "unset" — not as a provider named the empty string, which would fail
    /// every agent load.
    /// Test: itself.
    #[test]
    fn parse_treats_a_blank_value_as_unset() {
        assert_eq!(parse("[providers]\ndefault_provider_id = \"\"\n"), None);
        assert_eq!(parse("[providers]\ndefault_provider_id = \"   \"\n"), None);
        assert_eq!(parse("[providers]\n"), None);
    }

    /// Why: a half-edited `config.toml` must degrade to today's ambient
    /// behaviour, never brick every agent load in the harness.
    /// Test: itself.
    #[test]
    fn parse_tolerates_a_malformed_body() {
        assert_eq!(parse("[providers\ndefault_provider_id = "), None);
        assert_eq!(parse("[providers]\ndefault_provider_id = 7\n"), None);
    }

    /// Why: proves the documented path — `<home>/.trusty-agents/config.toml` —
    /// is the file actually read.
    /// Test: itself.
    #[test]
    fn read_from_home_finds_the_configured_provider() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join(".trusty-agents");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("config.toml"),
            "[providers]\ndefault_provider_id = \"bedrock\"\n",
        )
        .expect("write config");

        assert_eq!(read_from_home(home.path()).as_deref(), Some("bedrock"));
    }

    /// Why: no config file is the state of every existing install, and it must
    /// mean "unset", not an error.
    /// Test: itself.
    #[test]
    fn read_from_home_is_none_without_a_config_file() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_from_home(home.path()), None);
    }
}
