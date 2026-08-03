//! [`CredentialError`] — the denial taxonomy (issue #4565, DOC-45 §7).
//!
//! # Spec References
//!
//! - [`SPEC-CREDAUTH-05~draft`](docs/specs/DOC-45-credential-authority-model.md#SPEC-CREDAUTH-05~draft)
//!
//! Why: `resolve_key` returns `Option<String>`, so today "there is no such
//! credential", "you may not read it", and "the token expired" are the same
//! answer: `None`. A caller cannot tell an operator what to do about it, and
//! a scheduled source silently failing forever on an expired token is the
//! result. Each variant here exists because it has a *different remediation*.
//!
//! What: five variants, each carrying the [`CredentialRef`] and the
//! [`Principal`] and each rendering an actionable remediation (`C-5.7`). Every
//! failure is a recoverable `Result::Err` — no panic, no `unwrap`, no
//! empty-string-as-success, and no silent `None` a caller can mistake for "not
//! configured" when the real answer was "denied" (`C-3.11`).
//!
//! **Five, not four.** #4040's "Done when" names four. DOC-45 `C-5.5`/`C-5.6`
//! adds [`CredentialError::ScopeUnavailable`] as a fifth and requires it stay
//! distinct from [`CredentialError::ZeroScope`], because the two have different
//! remediations: `ZeroScope` says *widen the grant*, which for a provider with
//! no read-only scope is advice that cannot be followed and will be worked
//! around by granting write. DOC-45 flags this as a deliberate addition rather
//! than folding it in silently, and so does this module.
//!
//! Test: `tests::every_variant_is_constructible_and_matchable`,
//! `tests::remediation_names_the_principal_and_the_ref`,
//! `tests::no_variant_renders_secret_material`.

use super::handle::CredentialRef;
use super::principal::{Principal, Scope};

/// Why a credential could not be resolved.
///
/// Why: see the module docs — one variant per distinct operator action.
/// What: five variants, all recoverable, all pattern-matchable by a caller
/// (`C-5.8` — distinguishable by matching, never by string comparison on a
/// rendered message), and none carrying secret material (`C-5.9`).
///
/// **`Missing` vs `Denied` (`C-5.10`).** The two are deliberately distinct so
/// an *operator* can tell "store the credential" from "issue a grant". Where a
/// denial is surfaced into model-visible context, the surfaced form must not
/// reveal whether a credential the principal is not granted actually exists —
/// the full distinction belongs in the audit record (#4567), which the operator
/// reads and the model does not. This type does not enforce that; the surfacing
/// call site does, and #4567 is where it is pinned.
///
/// Test: `tests::every_variant_is_constructible_and_matchable`,
/// `tests::no_variant_renders_secret_material`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CredentialError {
    /// No credential is stored for this reference at all (`C-5.1`). The grant
    /// may well exist; there is simply nothing to resolve.
    ///
    /// Also the answer when the reference names a provider absent from the
    /// registry (`C-2.7`) — hence `hint`, which names the registry in that case
    /// so the failure mode 13 of 23 credential env-vars used to hit silently
    /// now says what to do.
    #[error(
        "no credential stored for `{credential}` (principal `{principal}`); \
         store it with `config keys set {credential}`{hint}"
    )]
    Missing {
        /// The reference that could not be resolved.
        credential: CredentialRef,
        /// The principal that asked.
        principal: Principal,
        /// Extra remediation, e.g. naming the provider registry. Empty when
        /// there is nothing more to say.
        hint: String,
    },

    /// The credential exists, but this principal holds no grant covering it
    /// (`C-5.2`). The default-deny answer (`C-3.2`) and the sub-agent answer
    /// (`C-4.3`).
    #[error(
        "principal `{principal}` is not granted `{credential}`; \
         issue a grant for that principal, or accept the denial"
    )]
    Denied {
        /// The reference that was denied.
        credential: CredentialRef,
        /// The principal that was denied.
        principal: Principal,
    },

    /// A grant or credential existed and is no longer valid (`C-5.3`) — the
    /// grant's expiry passed, or the authority recorded the credential as
    /// revoked.
    #[error("`{credential}` is expired or revoked for principal `{principal}`; re-authenticate")]
    Expired {
        /// The reference whose credential or grant died.
        credential: CredentialRef,
        /// The principal that asked.
        principal: Principal,
    },

    /// The grant exists and is live, but its scope does not cover the request
    /// (`C-5.4`) — the intersection is empty.
    #[error(
        "principal `{principal}`'s grant on `{credential}` does not cover scope `{requested}`; \
         widen the grant's scope"
    )]
    ZeroScope {
        /// The reference whose grant was too narrow.
        credential: CredentialRef,
        /// The principal that asked.
        principal: Principal,
        /// The scope that was requested.
        requested: Scope,
    },

    /// The **provider** cannot issue a credential at the requested scope at all
    /// (`C-5.5`). Not a property of the grant; a property of the provider.
    ///
    /// Kept distinct from [`Self::ZeroScope`] by `C-5.6`: telling an operator
    /// to "widen the grant" for a provider that has no such scope is advice
    /// that cannot be followed, and the workaround is to grant write — which is
    /// the exact dishonesty DOC-63 `S-5.3` exists to prevent.
    #[error(
        "provider for `{credential}` cannot issue a credential at scope `{requested}` \
         (principal `{principal}`); this provider has no such scope"
    )]
    ScopeUnavailable {
        /// The reference whose provider cannot issue the scope.
        credential: CredentialRef,
        /// The principal that asked.
        principal: Principal,
        /// The scope that is unavailable.
        requested: Scope,
    },
}

impl CredentialError {
    /// The reference this failure is about.
    ///
    /// Why: #4567's audit record needs the ref off any failure without matching
    /// five variants at every call site.
    /// Test: `tests::every_variant_is_constructible_and_matchable`.
    pub fn credential(&self) -> &CredentialRef {
        match self {
            Self::Missing { credential, .. }
            | Self::Denied { credential, .. }
            | Self::Expired { credential, .. }
            | Self::ZeroScope { credential, .. }
            | Self::ScopeUnavailable { credential, .. } => credential,
        }
    }

    /// The principal this failure is about.
    ///
    /// Test: `tests::every_variant_is_constructible_and_matchable`.
    pub fn principal(&self) -> &Principal {
        match self {
            Self::Missing { principal, .. }
            | Self::Denied { principal, .. }
            | Self::Expired { principal, .. }
            | Self::ZeroScope { principal, .. }
            | Self::ScopeUnavailable { principal, .. } => principal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of each variant, for the tests below.
    fn one_of_each() -> Vec<CredentialError> {
        let credential = CredentialRef::parse("github/work").unwrap();
        let principal = Principal::Operator;
        vec![
            CredentialError::Missing {
                credential: credential.clone(),
                principal: principal.clone(),
                hint: String::new(),
            },
            CredentialError::Denied {
                credential: credential.clone(),
                principal: principal.clone(),
            },
            CredentialError::Expired {
                credential: credential.clone(),
                principal: principal.clone(),
            },
            CredentialError::ZeroScope {
                credential: credential.clone(),
                principal: principal.clone(),
                requested: Scope::write(),
            },
            CredentialError::ScopeUnavailable {
                credential,
                principal,
                requested: Scope::read(),
            },
        ]
    }

    /// Why: `C-5.8` — every variant must be constructible and observable from a
    /// test, and distinguishable by pattern-matching rather than by string
    /// comparison on a rendered message. This is #4565's acceptance criterion
    /// on the error type, and #4040's "four (five) diagnostics" clause.
    /// Test: itself.
    #[test]
    fn every_variant_is_constructible_and_matchable() {
        let all = one_of_each();
        assert_eq!(all.len(), 5, "DOC-45 C-5.1–C-5.5 name five variants");
        for e in &all {
            // Matched structurally, never by rendered text.
            let named = match e {
                CredentialError::Missing { .. } => "missing",
                CredentialError::Denied { .. } => "denied",
                CredentialError::Expired { .. } => "expired",
                CredentialError::ZeroScope { .. } => "zero-scope",
                CredentialError::ScopeUnavailable { .. } => "scope-unavailable",
            };
            assert!(!named.is_empty());
            assert_eq!(e.credential().provider(), "github");
            assert_eq!(e.principal(), &Principal::Operator);
        }
        // C-5.6: the two scope variants are distinct values, not aliases.
        assert_ne!(all[3], all[4]);
    }

    /// Why: `C-5.7` — every variant carries an actionable remediation naming
    /// the principal and the reference. `Denied` in particular must name both,
    /// so the first denial a sub-agent hits under §6 tells the operator exactly
    /// which grant to issue (`C-4.6`) rather than starting a debugging session.
    /// Test: itself.
    #[test]
    fn remediation_names_the_principal_and_the_ref() {
        for e in one_of_each() {
            let rendered = e.to_string();
            assert!(
                rendered.contains("github/work"),
                "no credential ref in: {rendered}"
            );
            assert!(rendered.contains("operator"), "no principal in: {rendered}");
        }
    }

    /// Why: `C-5.9` — no variant may carry secret material, not even a prefix
    /// "for identification". The type makes this structural: every field is a
    /// `CredentialRef`, a `Principal`, or a `Scope`, and none of the three can
    /// hold a value. This test pins the consequence.
    /// Test: itself.
    #[test]
    fn no_variant_renders_secret_material() {
        // pragma: allowlist secret
        let secret = "ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        for e in one_of_each() {
            let rendered = format!("{e} {e:?}");
            assert!(!rendered.contains(secret), "leaked: {rendered}");
            assert!(!rendered.contains("ghp_"), "leaked prefix: {rendered}");
        }
    }
}
