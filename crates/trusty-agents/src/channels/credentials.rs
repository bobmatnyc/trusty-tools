//! Resolving a binding's `credential_ref` — a name, never a value, and never a
//! second resolver.
//!
//! Why: before #7427 a channel binding carried no credential at all; every send
//! used whichever process-global token was in the environment, so two
//! assistants bound to two workspaces could not send as different identities
//! and nothing in the binding recorded which credential it meant. A binding now
//! names one.
//!
//! What: the name is a [`CredentialRef`] — `provider` or `provider/qualifier`,
//! the credential authority's own naming unit — and resolution is
//! [`trusty_common::credentials::authority::resolve`] and nothing else. That is
//! deliberate: DOC-45 `C-3.3` and this repo's common-entry-point rule both say
//! a second credential-resolution path is a defect, so this module contributes
//! no tier logic of its own. The env → `.env.local` → secure-store precedence,
//! the registry check that refuses an unnameable provider, the [`Secret`]
//! wrapper that cannot be serialised or cloned, and the grant check #4566 will
//! add all arrive from there.
//!
//! A reference is also confined to the provider family of the adapter that will
//! use it — the caller passes the adapter's
//! [`credential_providers`](super::ChannelAdapter::credential_providers) list.
//! Without that, a Slack binding could name `github` or `openrouter` and
//! forward an unrelated credential to Slack. Confinement is by registry key
//! rather than by environment-variable name because a key is what the authority
//! resolves; there is no point in the grammar at which an arbitrary variable
//! name could be supplied.
//!
//! Per-assistant credential authority is a separate concern. DOC-63 §7.1b
//! (`docs/specs/DOC-63-okg-sources.md`) states that a channel's send/receive
//! credential and an OKG source's read-scoped credential are two separate
//! grants against the same provider, both routed through #4040, and neither
//! implies the other. This module resolves the channel grant only.
//!
//! Test: `channel_credential_ref_resolves_through_the_authority`,
//! `channel_credential_ref_is_confined_to_the_adapters_providers`,
//! `channel_binding_serialization_never_carries_a_token_value`.

// #7427: a binding names a credential; it never holds one, and never resolves
// one itself.
use super::ChannelError;
use trusty_common::credentials::{CredentialRef, Principal, Scope, Secret, ServiceId};

/// Identity this crate resolves channel credentials as.
///
/// Why: [`trusty_common::credentials::authority::resolve`] takes a principal so
/// that #4566's grant check has something to check. Until it lands the argument
/// changes nothing, which is exactly why it has to be right now — the signature
/// will not change when the check arrives.
fn principal() -> Principal {
    ServiceId::parse("trusty-agents").map_or(Principal::Operator, Principal::Service)
}

/// Parse and confine a reference without resolving it.
///
/// Why: `Binding::validate` runs on every load and every save, including for
/// assistants whose credentials are not present on this host. Resolving there
/// would make a saved configuration unloadable on a machine that merely lacks
/// the credential. This checks only that the reference is a name this adapter
/// is allowed to send as.
/// What: parses the [`CredentialRef`] grammar, then requires the provider
/// segment to appear in `allowed` — the adapter's own provider family. A
/// qualifier is preserved, so `slack/second-workspace` reaches a distinct store
/// row while staying confined to Slack.
/// Test: `channel_credential_ref_is_confined_to_the_adapters_providers`.
pub(crate) fn validate_credential_ref(
    reference: &str,
    allowed: &[&'static str],
    env_prefix: &str,
) -> Result<(), ChannelError> {
    let parsed = CredentialRef::parse(reference).map_err(|e| {
        ChannelError::Credential(format!("reference is not a credential name: {e}"))
    })?;
    if !allowed.contains(&parsed.provider()) {
        return Err(ChannelError::Credential(format!(
            "provider `{}` is not one this channel may send as (allowed: {})",
            parsed.provider(),
            allowed.join(", ")
        )));
    }
    // The allowlist above is by registry key; this is the property that makes
    // those keys safe, checked rather than assumed. A registry entry that
    // mapped an allowed key outside the provider's own variable family would
    // otherwise turn the allowlist into a hole.
    match trusty_common::credentials::env_var_for(parsed.provider()) {
        Some(var) if var.starts_with(env_prefix) => Ok(()),
        Some(var) => Err(ChannelError::Credential(format!(
            "provider `{}` resolves `{var}`, outside this channel's `{env_prefix}` credentials",
            parsed.provider()
        ))),
        None => Err(ChannelError::Credential(format!(
            "provider `{}` is not in the credential registry",
            parsed.provider()
        ))),
    }
}

/// Resolve a reference to the credential it names.
///
/// Why: resolution happens where the credential is consumed — at send time, in
/// the adapter — so nothing between the config file and the HTTP request holds
/// a value (DOC-45 `C-8.4`).
/// What: confines the reference, then hands it to the authority. A reference
/// naming an unregistered provider, or one no storage tier answers for, comes
/// back as [`ChannelError::Credential`] carrying the authority's own message.
/// There is no fallback to a process-global token: falling back would send as
/// an identity the binding did not name.
/// Test: `channel_credential_ref_resolves_through_the_authority`.
pub(crate) fn resolve_credential(
    reference: &str,
    allowed: &[&'static str],
    env_prefix: &str,
) -> Result<Secret<String>, ChannelError> {
    validate_credential_ref(reference, allowed, env_prefix)?;
    let parsed = CredentialRef::parse(reference).map_err(|e| {
        ChannelError::Credential(format!("reference is not a credential name: {e}"))
    })?;
    trusty_common::credentials::authority::resolve(&parsed, &principal(), &Scope::write())
        .map_err(|e| ChannelError::Credential(e.to_string()))
}

#[cfg(test)]
pub(crate) mod test_env {
    //! Save-and-restore guard for a credential environment variable.
    //!
    //! Why: the adapter send tests need a known token on the wire, and the
    //! authority's env tier is the only tier that can be set without writing to
    //! a real keychain or the operator's `0600` credential file. The guard
    //! restores whatever the machine actually had, so a developer's live token
    //! survives the test.

    /// Restores a variable's prior value on drop.
    pub(crate) struct EnvVarGuard {
        name: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        /// Set `name` to `value`, remembering what was there.
        pub(crate) fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var(name).ok();
            // SAFETY: every caller is `#[serial_test::serial(channel_credentials)]`, so no
            // other test in any process observes the window.
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: see `set`.
            match self.previous.take() {
                Some(value) => unsafe { std::env::set_var(self.name, value) },
                None => unsafe { std::env::remove_var(self.name) },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::server::agent_channels::Binding;
    use crate::channels::ChannelAdapter;
    use test_env::EnvVarGuard;

    const SLACK_PROVIDERS: &[&str] = &["slack", "slack-user", "slack-app"];

    #[test]
    #[serial_test::serial(channel_credentials)]
    fn channel_credential_ref_resolves_through_the_authority() {
        let _guard = EnvVarGuard::set("SLACK_APP_TOKEN", "xapp-7427-not-a-real-token");
        let resolved = resolve_credential("slack-app", SLACK_PROVIDERS, "SLACK_").unwrap();
        assert_eq!(resolved.expose(), "xapp-7427-not-a-real-token");
        // A resolved secret renders a constant, never the value.
        assert!(!format!("{resolved:?}").contains("xapp-7427"));
    }

    #[test]
    fn channel_credential_ref_is_confined_to_the_adapters_providers() {
        // The shape the security review named: a Slack binding must not be able
        // to forward an unrelated credential to Slack.
        for foreign in ["github", "openrouter", "anthropic", "aws"] {
            assert!(
                validate_credential_ref(foreign, SLACK_PROVIDERS, "SLACK_").is_err(),
                "`{foreign}` must not be sendable as a Slack credential"
            );
        }
        assert!(validate_credential_ref("slack", SLACK_PROVIDERS, "SLACK_").is_ok());
        assert!(
            validate_credential_ref("slack/second-workspace", SLACK_PROVIDERS, "SLACK_").is_ok()
        );
        assert!(validate_credential_ref("telegram", SLACK_PROVIDERS, "SLACK_").is_err());
        // Out-of-grammar text is not a credential name at all.
        assert!(validate_credential_ref("env:SLACK_BOT_TOKEN", SLACK_PROVIDERS, "SLACK_").is_err());
        assert!(
            validate_credential_ref("AWS_SECRET_ACCESS_KEY", SLACK_PROVIDERS, "SLACK_").is_err()
        );
        assert!(validate_credential_ref("", SLACK_PROVIDERS, "SLACK_").is_err());
    }

    /// Every key an adapter may send as maps to an environment variable under
    /// that adapter's own prefix.
    ///
    /// Why: the confinement above is by registry key, so it holds only while
    /// the registry keeps mapping those keys to that provider's variables. This
    /// asserts the property the security review asked for directly, and fails
    /// if a future registry entry breaks it.
    #[test]
    fn channel_credential_providers_map_to_the_adapters_env_prefix() {
        for adapter in [
            &crate::channels::slack::SlackAdapter as &dyn ChannelAdapter,
            &crate::channels::telegram::TelegramAdapter,
        ] {
            let prefix = adapter.credential_env_prefix();
            for key in adapter.credential_providers() {
                let var = trusty_common::credentials::env_var_for(key)
                    .unwrap_or_else(|| panic!("`{key}` is not in the credential registry"));
                assert!(
                    var.starts_with(prefix),
                    "`{key}` maps to `{var}`, outside `{prefix}` for provider `{}`",
                    adapter.provider()
                );
            }
        }
    }

    #[test]
    #[serial_test::serial(channel_credentials)]
    fn channel_binding_serialization_never_carries_a_token_value() {
        let _guard = EnvVarGuard::set("SLACK_APP_TOKEN", "xapp-7427-not-a-real-token");
        let binding: Binding = serde_json::from_value(serde_json::json!({
            "id":"team","name":"Team","provider":"slack","target":"C123456",
            "enabled":true,"send_enabled":true,"credential_ref":"slack-app",
        }))
        .unwrap();
        assert!(binding.validate().is_ok());
        assert_eq!(
            resolve_credential(
                binding.credential_ref.as_deref().unwrap(),
                SLACK_PROVIDERS,
                "SLACK_"
            )
            .unwrap()
            .expose(),
            "xapp-7427-not-a-real-token"
        );

        // The on-disk format is `<assistant>.channels.json`; `Debug` is the
        // other way a binding reaches a log line.
        let compact = serde_json::to_string(&binding).unwrap();
        let pretty = serde_json::to_string_pretty(&binding).unwrap();
        for rendered in [&compact, &pretty, &format!("{binding:?}")] {
            assert!(
                !rendered.contains("xapp-7427"),
                "binding rendering leaked the credential value"
            );
            assert!(
                rendered.contains("slack-app"),
                "binding rendering dropped the credential reference"
            );
        }
    }
}
