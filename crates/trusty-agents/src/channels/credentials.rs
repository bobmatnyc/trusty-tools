//! Resolving a binding's `credential_ref` — a name, never a value.
//!
//! Why: before #7427 a channel binding carried no credential at all; every send
//! used whichever process-global token happened to be in the environment
//! (`SLACK_BOT_TOKEN`, `TELEGRAM_BOT_TOKEN`), so two assistants bound to two
//! workspaces could not send as different identities, and nothing in the
//! binding recorded which credential it meant. A binding now names one. The
//! name is what the config file holds; the value is resolved at send time and
//! returned in a [`Secret`], which cannot be serialised, cloned, or printed —
//! so "the assistant home holds no credential" is a property of the types.
//!
//! What: a reference is `<scheme>:<name>`. This module implements `env:` — the
//! process environment, with `.env.local` folded in by
//! [`trusty_common::credentials::load_env_local_once`] so the precedence
//! matches the shared resolver's first two tiers. Any other scheme
//! (`gworkspace:<account>` among them) is an error, deliberately: falling back
//! to the process-global token would send as an identity the binding did not
//! name. A binding with no `credential_ref` keeps the pre-#7427 behaviour and
//! uses the provider's global token.
//!
//! Per-assistant credential authority is a separate concern. DOC-63 §7.1b
//! (`docs/specs/DOC-63-okg-sources.md`) states that a channel's send/receive
//! credential and an OKG source's read-scoped credential are two separate
//! grants against the same provider, both routed through #4040, and neither
//! implies the other. This module resolves the channel grant only.
//!
//! Test: `channel_credential_ref_resolves_env_scheme_and_rejects_others`,
//! `channel_binding_serialization_never_carries_a_token_value`.

// #7427: a binding names a credential; it never holds one.
use super::ChannelError;
use trusty_common::credentials::Secret;

/// The one credential scheme implemented today.
const ENV_SCHEME: &str = "env";

/// Longest accepted `<scheme>:<name>` reference, in bytes.
const MAX_REF_LEN: usize = 128;

/// Split a reference into `(scheme, name)`, rejecting anything out of grammar.
///
/// Why: one parser, so validation at save time and resolution at send time can
/// never disagree about what a reference means.
fn split(reference: &str) -> Result<(&str, &str), ChannelError> {
    if reference.len() > MAX_REF_LEN {
        return Err(ChannelError::Credential(format!(
            "reference is too long ({} bytes; max {MAX_REF_LEN})",
            reference.len()
        )));
    }
    let (scheme, name) = reference.split_once(':').ok_or_else(|| {
        ChannelError::Credential("reference must be `<scheme>:<name>`".to_string())
    })?;
    if name.is_empty() {
        return Err(ChannelError::Credential(
            "reference must be `<scheme>:<name>`".to_string(),
        ));
    }
    Ok((scheme, name))
}

/// Check a reference's shape without resolving it.
///
/// Why: `Binding::validate` runs on every load and every save, including for
/// assistants whose credentials are not present on this host. Resolving there
/// would make a saved configuration unloadable on a machine that merely lacks
/// the environment variable. This checks only that the reference is a name this
/// build knows how to resolve.
/// What: accepts `env:<NAME>` where `<NAME>` is `[A-Z][A-Z0-9_]*`. Rejects an
/// unknown scheme, so a `gworkspace:` reference cannot be saved before the
/// scheme exists and then silently send with the global token.
/// Test: `channel_credential_ref_resolves_env_scheme_and_rejects_others`.
pub(crate) fn validate_credential_ref(reference: &str) -> Result<(), ChannelError> {
    let (scheme, name) = split(reference)?;
    if scheme != ENV_SCHEME {
        return Err(ChannelError::Credential(format!(
            "unknown credential scheme `{scheme}`; this build resolves `{ENV_SCHEME}:` only"
        )));
    }
    let valid = name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(ChannelError::Credential(
            "`env:` name must be an uppercase environment variable name".to_string(),
        ));
    }
    Ok(())
}

/// Resolve a reference to the credential it names.
///
/// Why: resolution happens where the credential is consumed — at send time, in
/// the adapter — so nothing between the config file and the HTTP request holds
/// a value.
/// What: validates the shape, folds `.env.local` into the process environment
/// through the shared loader, and reads the named variable. A missing variable
/// is an error naming the variable, never a fallback to the provider's global
/// token.
/// Test: `channel_credential_ref_resolves_env_scheme_and_rejects_others`.
pub(crate) fn resolve_credential(reference: &str) -> Result<Secret<String>, ChannelError> {
    validate_credential_ref(reference)?;
    let (_, name) = split(reference)?;
    trusty_common::credentials::load_env_local_once();
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(Secret::new(value)),
        _ => Err(ChannelError::Credential(format!(
            "environment variable `{name}` is not set"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::server::agent_channels::Binding;

    /// Env var this test owns outright, so no sibling test races it.
    const TEST_VAR: &str = "TRUSTY_AGENTS_TEST_CHANNEL_TOKEN_7427";
    const TEST_VALUE: &str = "xoxb-7427-not-a-real-token";

    /// Set [`TEST_VAR`] for the duration of the test.
    ///
    /// Why: `std::env::set_var` is process-global and `unsafe` in edition 2024.
    /// The variable name is unique to this test and both call sites are
    /// `#[serial_test::serial]`, so no other test observes the window.
    fn set_test_var() {
        // SAFETY: single-threaded section of a `#[serial]` test; the name is
        // used by this module's tests only.
        unsafe { std::env::set_var(TEST_VAR, TEST_VALUE) };
    }

    fn clear_test_var() {
        // SAFETY: see `set_test_var`.
        unsafe { std::env::remove_var(TEST_VAR) };
    }

    #[test]
    #[serial_test::serial]
    fn channel_credential_ref_resolves_env_scheme_and_rejects_others() {
        set_test_var();
        let resolved = resolve_credential(&format!("env:{TEST_VAR}")).unwrap();
        assert_eq!(resolved.expose(), TEST_VALUE);
        // A resolved secret renders a constant, never the value.
        assert!(!format!("{resolved:?}").contains(TEST_VALUE));

        assert!(validate_credential_ref("gworkspace:masa").is_err());
        assert!(validate_credential_ref("SLACK_BOT_TOKEN").is_err());
        assert!(validate_credential_ref("env:").is_err());
        assert!(validate_credential_ref("env:lowercase").is_err());
        assert!(validate_credential_ref(&format!("env:{}", "A".repeat(200))).is_err());
        // An unset variable is an error, not a fallback to the global token.
        clear_test_var();
        assert!(resolve_credential(&format!("env:{TEST_VAR}")).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn channel_binding_serialization_never_carries_a_token_value() {
        set_test_var();
        let binding: Binding = serde_json::from_value(serde_json::json!({
            "id":"team","name":"Team","provider":"slack","target":"C123456",
            "enabled":true,"send_enabled":true,
            "credential_ref":format!("env:{TEST_VAR}"),
        }))
        .unwrap();
        assert!(binding.validate().is_ok());
        assert_eq!(
            resolve_credential(binding.credential_ref.as_deref().unwrap())
                .unwrap()
                .expose(),
            TEST_VALUE
        );

        // The on-disk format is `<assistant>.channels.json`; `Debug` is the
        // other way a binding reaches a log line.
        let compact = serde_json::to_string(&binding).unwrap();
        let pretty = serde_json::to_string_pretty(&binding).unwrap();
        for rendered in [&compact, &pretty, &format!("{binding:?}")] {
            assert!(
                !rendered.contains(TEST_VALUE),
                "binding rendering leaked the credential value"
            );
            assert!(
                rendered.contains(TEST_VAR),
                "binding rendering dropped the credential reference"
            );
        }
        clear_test_var();
    }
}
