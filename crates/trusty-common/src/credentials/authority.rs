//! The single credential-resolution entry point, resolved **at use time**
//! (issue #4565, DOC-45 `C-3.3`/`C-3.4`/`C-8.4`).
//!
//! # Spec References
//!
//! - [`SPEC-CREDAUTH-03~draft`](docs/specs/DOC-45-credential-authority-model.md#SPEC-CREDAUTH-03~draft)
//!
//! Why: `C-3.3` requires exactly one entry point, and `C-8.4` requires
//! resolution to happen at use time — *"never at config-load time and never at
//! spawn-list-construction time"* — so the window in which a secret exists in
//! memory is bounded by the call that needs it. Those two clauses only bite if
//! the *shape* of the API makes the eager version awkward and the lazy version
//! natural, which is what this module is for.
//!
//! ## How use-time resolution is enforced, not merely enabled
//!
//! 1. **A config struct cannot hold the result.** [`Secret`] implements neither
//!    `Serialize` nor `Deserialize`, and every config type in this workspace
//!    derives both, so a `Secret` field fails to compile inside one. The only
//!    credential-shaped thing a config row *can* hold is a
//!    [`CredentialRef`], which is exactly `C-8.8`. Pinned at compile time in
//!    `super::secret::not_serialize_not_clone`.
//! 2. **The result cannot be duplicated into a longer-lived home.** [`Secret`]
//!    is not `Clone` and [`Secret::expose`] returns a borrow, so caching one is
//!    a deliberate move, not an accident.
//! 3. **Resolution needs facts a config loader does not have.** [`resolve`]
//!    takes a [`Principal`] and a [`Scope`]: *who is acting* and *what for*.
//!    Neither is answerable at config-load time — a loader has no principal in
//!    hand and no operation in flight — so the eager call is not merely
//!    discouraged, it has no arguments.
//! 4. **The caller can avoid seeing the value at all.** [`resolve_client`]
//!    hands back an already-authenticated handle built from the secret, so a
//!    source type receives *"a resolved, scoped client and nothing else"*
//!    (`C-3.4`, DOC-63 `S-5.1`). A caller that never sees the string cannot
//!    leak the string.
//!
//! ## What this module does NOT do — the honesty clause
//!
//! **There is no authorization here.** [`resolve`] takes a `Principal` and
//! checks no grant against it, because grants, the ACL, default-deny, and the
//! code-owned floor are #4566 (DOC-45 §5) and #4565 is explicitly scoped to the
//! reference type and the signature. Today's behaviour is therefore identical
//! to [`super::resolve_key`]'s: the three storage tiers answer, and any caller
//! that can reach the function gets the value. `C-3.12` says the same thing
//! about default-deny at large — it is not airtight until #4571 migrates the 55
//! raw `std::env::var` reads, and this document does not claim it is.
//!
//! What #4565 buys is that the signature will not change when #4566 lands: the
//! grant check goes *inside* [`resolve`], and no consumer is migrated twice.
//!
//! Test: `tests::oauth_and_api_key_shapes_resolve_through_one_entry_point`,
//! `tests::unregistered_provider_is_missing_and_names_the_registry`,
//! `tests::absent_credential_is_missing`,
//! `tests::resolve_client_never_hands_back_the_string`,
//! `tests::resolved_secret_does_not_render_in_debug_or_display`.

use super::error::CredentialError;
use super::handle::CredentialRef;
use super::principal::{Principal, Scope};
use super::registry::env_var_for;
use super::secret::Secret;
use super::{KeyStore, default_store, dotenv, resolve_key_with};

/// Resolve `credential` for `principal` at `scope`, at the point of use.
///
/// Why: the one entry point `C-3.3` requires. A second resolution path is a
/// defect under this repo's common-entry-point rule and under DOC-63 `S-5.2`,
/// and is rejected at review regardless of expedience.
///
/// What: loads `.env.local` once, then delegates to [`resolve_with`] against
/// [`default_store`]. Returns the value wrapped in a [`Secret`], which cannot
/// be serialised, cloned, or printed.
///
/// **Call this where the credential is consumed** — inside the function that
/// builds the request, not in the constructor that builds the config. `C-8.4`.
///
/// # Errors
///
/// [`CredentialError::Missing`] when the reference names an unregistered
/// provider (`C-2.7`, with a remediation naming the registry) or when no tier
/// holds a value. The other four variants become reachable when #4566 adds the
/// grant check; see the module docs.
///
/// Test: `tests::oauth_and_api_key_shapes_resolve_through_one_entry_point`
/// exercises the hermetic core; the `load_env_local_once` + `default_store`
/// wiring is intentionally not independently unit tested, for the same reason
/// [`super::resolve_key`]'s is not.
pub fn resolve(
    credential: &CredentialRef,
    principal: &Principal,
    scope: &Scope,
) -> Result<Secret<String>, CredentialError> {
    dotenv::load_env_local_once();
    resolve_with(credential, principal, scope, default_store().as_ref())
}

/// Hermetic core of [`resolve`]: same decision, injectable store.
///
/// Why: separated so tests can inject a `MemoryKeyStore` and control the
/// process environment without touching the real filesystem, `$HOME`, or an OS
/// keychain — the same split [`super::resolve_key`] / [`super::resolve_key_with`]
/// already uses. It is one entry point with an injectable dependency, not a
/// second entry point.
///
/// What: checks the provider registry (`C-2.7`), then the 3-tier precedence.
/// `scope` is accepted and carried into the error path but not yet checked
/// against a grant — see the module's honesty clause.
///
/// # Errors
///
/// [`CredentialError::Missing`], as [`resolve`].
///
/// Test: `tests::oauth_and_api_key_shapes_resolve_through_one_entry_point`,
/// `tests::unregistered_provider_is_missing_and_names_the_registry`,
/// `tests::absent_credential_is_missing`.
pub fn resolve_with(
    credential: &CredentialRef,
    principal: &Principal,
    scope: &Scope,
    store: &dyn KeyStore,
) -> Result<Secret<String>, CredentialError> {
    // C-2.7: a ref, a registry entry, and a storage location are one chain.
    // A ref naming an unregistered provider fails with `Missing` carrying a
    // remediation that names the registry — the failure mode that used to hit
    // silently for 13 of this workspace's 23 credential env vars.
    if env_var_for(credential.provider()).is_none() {
        return Err(CredentialError::Missing {
            credential: credential.clone(),
            principal: principal.clone(),
            hint: format!(
                " — provider `{}` is not in the credential registry \
                 (`trusty_common::credentials::registry::REGISTRY`, #4564)",
                credential.provider()
            ),
        });
    }

    // #4566 inserts the grant check here: `effective(principal)` per C-3.7,
    // then `Scope::covers` per C-3.9, before any secret is materialised.
    let _ = scope;

    match resolve_key_with(&store_key(credential), store) {
        Some(value) => Ok(Secret::new(value)),
        None => Err(CredentialError::Missing {
            credential: credential.clone(),
            principal: principal.clone(),
            hint: String::new(),
        }),
    }
}

/// The key a reference is stored and looked up under.
///
/// Why: an unqualified ref must keep hitting exactly the key the pre-#4565
/// resolver used, or every credential already in a store or an environment
/// variable becomes unreachable the day this lands. A qualified ref must reach
/// a *distinct* row, or `github/work` and `github/personal` collapse onto one
/// credential.
/// What: renders the whole ref. The consequence for the env tier is deliberate
/// and worth stating: [`super::registry::env_var_for`] has no entry for
/// `github/work`, so a qualified ref resolves from the store only — there is
/// one canonical environment variable per provider, and a second credential of
/// the same provider has nowhere in the environment to live.
/// Test: `tests::qualified_refs_reach_distinct_store_rows`.
fn store_key(credential: &CredentialRef) -> String {
    credential.to_string()
}

/// A type that can be built from a resolved credential without the caller ever
/// holding the value (`C-3.4`).
///
/// Why: DOC-63 `S-5.1` requires that a source type receives *"a resolved,
/// scoped client and nothing else"*. The point is not convenience — a caller
/// that never sees the string cannot leak the string into a log, a
/// `ToolResult`, or a delegation payload.
/// What: implemented by an authenticated client handle. The `Secret` is lent
/// for the duration of the call and dropped by [`resolve_client`] before it
/// returns.
/// Test: `tests::resolve_client_never_hands_back_the_string`.
pub trait FromCredential: Sized {
    /// Build `Self` from the resolved credential.
    ///
    /// # Errors
    ///
    /// A [`CredentialError`] the implementor judges appropriate — typically
    /// [`CredentialError::Expired`] for a token the provider rejects at
    /// construction time.
    fn from_credential(
        secret: &Secret<String>,
        credential: &CredentialRef,
        principal: &Principal,
    ) -> Result<Self, CredentialError>;
}

/// Resolve and immediately build an authenticated handle (`C-3.4`).
///
/// Why: the shape DOC-63 `S-5.1` asks for. Same entry point, same registry
/// check, same errors — this is [`resolve`] with the value consumed in place
/// rather than returned, not a second resolution path.
/// What: resolves, hands the [`Secret`] to `T::from_credential` by reference,
/// and drops it. `T` never has to be a secret-bearing type.
///
/// # Errors
///
/// Whatever [`resolve`] returns, or whatever `T::from_credential` returns.
///
/// Test: `tests::resolve_client_never_hands_back_the_string`.
pub fn resolve_client<T: FromCredential>(
    credential: &CredentialRef,
    principal: &Principal,
    scope: &Scope,
) -> Result<T, CredentialError> {
    let secret = resolve(credential, principal, scope)?;
    T::from_credential(&secret, credential, principal)
}

/// Store-injectable counterpart of [`resolve_client`], for tests.
///
/// Test: `tests::resolve_client_never_hands_back_the_string`.
///
/// # Errors
///
/// Whatever [`resolve_with`] returns, or whatever `T::from_credential` returns.
pub fn resolve_client_with<T: FromCredential>(
    credential: &CredentialRef,
    principal: &Principal,
    scope: &Scope,
    store: &dyn KeyStore,
) -> Result<T, CredentialError> {
    let secret = resolve_with(credential, principal, scope, store)?;
    T::from_credential(&secret, credential, principal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::MemoryKeyStore;
    use serial_test::serial;

    /// A stand-in for an authenticated client: it keeps a derived, non-secret
    /// fact about the credential and drops the value.
    struct AuthenticatedClient {
        /// The reference the client was built for — non-secret by `C-2.1`.
        credential: String,
        /// Length of the credential, to prove the builder really saw it. A
        /// length is not a substring, so this leaks nothing.
        credential_len: usize,
    }

    impl FromCredential for AuthenticatedClient {
        fn from_credential(
            secret: &Secret<String>,
            credential: &CredentialRef,
            _principal: &Principal,
        ) -> Result<Self, CredentialError> {
            Ok(Self {
                credential: credential.to_string(),
                credential_len: secret.expose().len(),
            })
        }
    }

    /// Why: DOC-63 §7.1b item 5 warns that the plain-API-key shape is the one
    /// most likely to be special-cased, and `C-2.5`/`C-3.3`/`C-8.5` require both
    /// shapes to traverse the *same* entry point with no type, trait, or code
    /// path existing for only one of them. This test is that requirement:
    /// `google-oauth` (OAuth) and `brave` (plain API key) resolve through one
    /// call with one argument list.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn oauth_and_api_key_shapes_resolve_through_one_entry_point() {
        let store = MemoryKeyStore::new();
        // pragma: allowlist secret
        store
            .set("google-oauth", "oauth-refresh-token-value")
            .unwrap();
        // pragma: allowlist secret
        store.set("brave", "plain-api-key-value").unwrap();
        // SAFETY: `#[serial(dotenv_credential_env)]` guarantees no other test in
        // this crate mutates process env concurrently with this one.
        unsafe {
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_SECRET");
            std::env::remove_var("BRAVE_API_KEY");
        }

        let principal = Principal::Operator;
        let scope = Scope::read();
        for (name, expected) in [
            ("google-oauth", "oauth-refresh-token-value"),
            ("brave", "plain-api-key-value"),
        ] {
            let cred = CredentialRef::parse(name).unwrap();
            let secret = resolve_with(&cred, &principal, &scope, &store).unwrap();
            assert_eq!(secret.expose(), expected, "shape {name} did not resolve");
        }
    }

    /// Why: `C-2.7` — a ref naming a provider absent from the registry must
    /// fail with `Missing` carrying a remediation that names the registry,
    /// because "silently" is how 13 of 23 credential env vars used to fail.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn unregistered_provider_is_missing_and_names_the_registry() {
        let store = MemoryKeyStore::new();
        // pragma: allowlist secret
        store
            .set("not-a-provider", "value-that-must-not-be-returned")
            .unwrap();
        let cred = CredentialRef::parse("not-a-provider").unwrap();
        let err = resolve_with(&cred, &Principal::Operator, &Scope::read(), &store).unwrap_err();
        assert!(matches!(err, CredentialError::Missing { .. }));
        let rendered = err.to_string();
        assert!(
            rendered.contains("registry"),
            "no registry hint: {rendered}"
        );
        assert!(rendered.contains("not-a-provider"), "no ref: {rendered}");
        assert!(
            !rendered.contains("value-that-must-not-be-returned"),
            "leaked the stored value: {rendered}"
        );
    }

    /// Why: `C-3.11` — a resolution failure is a recoverable `Err`, never a
    /// silent `None` a caller can mistake for "not configured".
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn absent_credential_is_missing() {
        // SAFETY: see `oauth_and_api_key_shapes_resolve_through_one_entry_point`.
        unsafe {
            std::env::remove_var("LINEAR_API_KEY");
        }
        let store = MemoryKeyStore::new();
        let cred = CredentialRef::parse("linear").unwrap();
        let err = resolve_with(&cred, &Principal::Operator, &Scope::read(), &store).unwrap_err();
        assert!(matches!(err, CredentialError::Missing { hint, .. } if hint.is_empty()));
    }

    /// Why: `C-3.4` / DOC-63 `S-5.1` — the authority must be able to hand back
    /// an already-authenticated handle so the caller never touches the value.
    /// The handle here carries only non-secret derived facts, and the assertion
    /// is that nothing about it renders the credential.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn resolve_client_never_hands_back_the_string() {
        let store = MemoryKeyStore::new();
        // pragma: allowlist secret
        let value = "sk-brave-abcdefghijklmnop";
        store.set("brave", value).unwrap();
        // SAFETY: see `oauth_and_api_key_shapes_resolve_through_one_entry_point`.
        unsafe {
            std::env::remove_var("BRAVE_API_KEY");
        }
        let cred = CredentialRef::parse("brave").unwrap();
        let client: AuthenticatedClient =
            resolve_client_with(&cred, &Principal::Operator, &Scope::read(), &store).unwrap();
        assert_eq!(client.credential, "brave");
        assert_eq!(client.credential_len, value.len());
    }

    /// Why: the end-to-end statement of `C-8.2` on the *resolved* value, not
    /// just on a hand-constructed `Secret` — this is what a caller who logs the
    /// result of `resolve` would actually get.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn resolved_secret_does_not_render_in_debug_or_display() {
        let store = MemoryKeyStore::new();
        // pragma: allowlist secret
        let value = concat!("xo", "xb", "-2314151234-2321313111-QwErTyUiOpAsDf");
        store.set("slack", value).unwrap();
        // SAFETY: see `oauth_and_api_key_shapes_resolve_through_one_entry_point`.
        unsafe {
            std::env::remove_var("SLACK_BOT_TOKEN");
        }
        let cred = CredentialRef::parse("slack").unwrap();
        let secret = resolve_with(&cred, &Principal::Operator, &Scope::read(), &store).unwrap();
        let rendered = format!("{secret} {secret:?} {cred}");
        assert!(!rendered.contains(value), "leaked: {rendered}");
        assert!(
            !rendered.contains(concat!("xo", "xb")),
            "leaked prefix: {rendered}"
        );
        // C-2.6: the *handle* still prints verbatim, which is the whole point.
        assert!(
            rendered.contains("slack"),
            "handle should print: {rendered}"
        );
    }

    /// Why: a qualified ref must reach a distinct store row, or `github/work`
    /// and `github/personal` collapse onto one credential.
    /// Test: itself.
    #[test]
    #[serial(dotenv_credential_env)]
    fn qualified_refs_reach_distinct_store_rows() {
        let store = MemoryKeyStore::new();
        // pragma: allowlist secret
        store.set("github/work", "work-token").unwrap();
        // pragma: allowlist secret
        store.set("github/personal", "personal-token").unwrap();
        // SAFETY: see `oauth_and_api_key_shapes_resolve_through_one_entry_point`.
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
        }
        let principal = Principal::Operator;
        let scope = Scope::read();
        let work = CredentialRef::parse("github/work").unwrap();
        let personal = CredentialRef::parse("github/personal").unwrap();
        assert_eq!(
            resolve_with(&work, &principal, &scope, &store)
                .unwrap()
                .expose(),
            "work-token"
        );
        assert_eq!(
            resolve_with(&personal, &principal, &scope, &store)
                .unwrap()
                .expose(),
            "personal-token"
        );
    }
}
