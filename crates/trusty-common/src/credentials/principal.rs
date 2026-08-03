//! [`Principal`] and [`Scope`] — the two arguments that make resolution a
//! *decision* rather than a lookup (issue #4565, DOC-45 §3 and §5.4).
//!
//! # Spec References
//!
//! - [`SPEC-CREDAUTH-01~draft`](docs/specs/DOC-45-credential-authority-model.md#SPEC-CREDAUTH-01~draft)
//!
//! Why: #4565's job is to fix the *signature* of `resolve` before #4566 fills
//! in the authorization behind it, so that no consumer has to be migrated twice.
//! Both types therefore exist here in their final shape and are deliberately
//! inert: nothing in this module denies anything.
//!
//! **Scope boundary, stated plainly.** ACL, grants, default-deny, and the
//! code-owned floor are #4566 and are **not** implemented here. What is here is
//! the vocabulary those decisions will be expressed in.
//!
//! **What is deliberately missing.** DOC-45 `C-1.3` is PROVISIONAL pending owner
//! question **Q-B** (do two assistant instances share a credential namespace?)
//! and says in terms: *"Do not implement `Principal`'s `Assistant` variant until
//! Q-B is answered."* `C-12.1` repeats it as a delivery rule. So [`Principal`]
//! ships with `Operator` and `Service` — the two kinds Q-B does not touch — and
//! is `#[non_exhaustive]` so `Assistant` and `SubAgent` can be added by #4566
//! without a breaking change.
//!
//! Test: `tests::principal_renders_its_full_shape`,
//! `tests::service_id_rejects_out_of_grammar_text`,
//! `tests::scope_read_is_covered_by_write`,
//! `tests::provider_scopes_must_all_be_covered`.

use std::fmt;

use super::handle::CredentialRefError;

/// The thing an authorization decision is made *about* (`C-1.1`).
///
/// Why: `resolve_key(provider)` today takes a provider name **and nothing
/// else**, so any code in any crate that can call it gets any credential the
/// process can see — that is the entire access-control model as of #4565.
/// Threading a `Principal` through the entry point is the change that makes an
/// ACL expressible at all; #4566 is what makes it enforced.
///
/// What: a **closed enumeration** (`C-1.7`), never a string — a stringly-typed
/// principal would let any caller spell one, re-opening `C-1.2`'s
/// derived-not-declared rule, and would make `C-3.7`'s exhaustive-match floor
/// impossible to write. `#[non_exhaustive]` because two of the four kinds
/// DOC-45 names are blocked on owner Q-B; see the module docs.
///
/// **Derivation (`C-1.2`).** A `Principal` is derived by the authority from the
/// executing context, never declared by the thing it names. Nothing in this
/// module enforces that yet — there is no authorization here to enforce it
/// *for* — and #4566 owns making it true.
///
/// **No secret material (`C-1.8`).** `Principal` renders its full shape in
/// `Debug`/`Display`, which is what makes it safe to place on every audit
/// record (#4567) without a redaction pass.
///
/// Test: `tests::principal_renders_its_full_shape`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Principal {
    /// The human at the keyboard — the authenticated owner of the machine.
    /// The highest authority, and per `C-3.6` the only kind that may issue or
    /// revoke a grant.
    Operator,

    /// A daemon or non-assistant process: `trusty-search`, `trusty-memory`,
    /// the `tm` daemon, a `trusty-code` session. Required by the owner's
    /// cross-product decision — trusty-code has no assistant in the loop at
    /// all, so without this kind the model would not apply to half of DOC-45's
    /// declared scope.
    Service(ServiceId),
}

impl fmt::Display for Principal {
    /// Render the full shape — no redaction, because there is nothing to
    /// redact (`C-1.8`).
    /// Test: `tests::principal_renders_its_full_shape`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operator => f.write_str("operator"),
            Self::Service(id) => write!(f, "service:{id}"),
        }
    }
}

/// A stable identifier for a [`Principal::Service`].
///
/// Why: "stable" is the load-bearing word — a grant is issued against it and
/// must survive a restart, so it cannot be a PID or a session id.
/// What: a validated newtype over the same lowercase-kebab grammar
/// [`super::CredentialRef`] uses, so a service id cannot carry secret material
/// into an audit record either.
/// Test: `tests::service_id_rejects_out_of_grammar_text`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceId(String);

impl ServiceId {
    /// Validate and wrap a service identifier.
    ///
    /// What: accepts `[a-z0-9]([a-z0-9-]*[a-z0-9])?`, at most 32 bytes —
    /// reusing [`super::CredentialRef`]'s segment grammar so the two cannot
    /// drift.
    /// Test: `tests::service_id_rejects_out_of_grammar_text`.
    pub fn parse(text: &str) -> Result<Self, CredentialRefError> {
        // A single-segment ref is exactly the grammar a service id needs;
        // routing through it keeps one validator rather than two.
        let as_ref = super::CredentialRef::parse(text)?;
        if as_ref.qualifier().is_some() {
            return Err(CredentialRefError::Shape);
        }
        Ok(Self(text.to_string()))
    }

    /// The identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The read/write dimension of a [`Scope`] (`C-3.8`).
///
/// Why: DOC-45 `C-3.10` exists because a false read-only claim has already been
/// shipped in this repo and caught. Making read-vs-write a required, typed part
/// of every resolution is the smallest thing that stops the next one.
/// Test: `tests::scope_read_is_covered_by_write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Access {
    /// Read-only use of the credential.
    Read,
    /// Read and write.
    Write,
}

/// What a resolution asks for, and what a grant permits (`C-3.8`).
///
/// Why: `C-3.9` requires scope to be checked **at the point of resolution**,
/// before any secret is materialised — so the scope has to be an argument to
/// `resolve`, not something the caller applies afterwards. It is also the
/// argument that makes eager, config-load-time resolution unnatural: at config
/// load there is no answer to "what am I about to do with this".
///
/// What: an [`Access`] level plus zero or more provider-native scope strings
/// (OAuth scopes, where the provider expresses them).
///
/// Test: `tests::scope_read_is_covered_by_write`,
/// `tests::provider_scopes_must_all_be_covered`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    access: Access,
    provider_scopes: Vec<String>,
}

impl Scope {
    /// A read-only scope with no provider-native scopes.
    pub fn read() -> Self {
        Self {
            access: Access::Read,
            provider_scopes: Vec::new(),
        }
    }

    /// A read-write scope with no provider-native scopes.
    pub fn write() -> Self {
        Self {
            access: Access::Write,
            provider_scopes: Vec::new(),
        }
    }

    /// Attach provider-native scope strings (OAuth scopes).
    ///
    /// Test: `tests::provider_scopes_must_all_be_covered`.
    pub fn with_provider_scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.provider_scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// The read/write dimension.
    pub fn access(&self) -> Access {
        self.access
    }

    /// The provider-native scope strings, if any.
    pub fn provider_scopes(&self) -> &[String] {
        &self.provider_scopes
    }

    /// Whether a grant at `self` covers a request for `requested`.
    ///
    /// Why: #4566 needs one definition of "covers" or the floor and the grant
    /// check will disagree. Defining it next to the type, before there is an
    /// ACL to use it, is what stops a second definition appearing inside the
    /// ACL later.
    /// What: `Write` covers `Read`; `Read` does not cover `Write`; and every
    /// provider-native scope the request names must appear in the grant. An
    /// empty grant scope list means "no provider-native scopes granted", so it
    /// covers only a request that names none — the fail-closed reading, matching
    /// `C-3.2`.
    /// Test: `tests::scope_read_is_covered_by_write`,
    /// `tests::provider_scopes_must_all_be_covered`.
    pub fn covers(&self, requested: &Scope) -> bool {
        if requested.access == Access::Write && self.access == Access::Read {
            return false;
        }
        requested
            .provider_scopes
            .iter()
            .all(|s| self.provider_scopes.contains(s))
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let access = match self.access {
            Access::Read => "read",
            Access::Write => "write",
        };
        if self.provider_scopes.is_empty() {
            f.write_str(access)
        } else {
            write!(f, "{access}[{}]", self.provider_scopes.join(" "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `C-1.8` — a `Principal` must render its full shape, because the
    /// audit record (#4567) places it verbatim and a redacted principal is an
    /// unattributable record.
    /// Test: itself.
    #[test]
    fn principal_renders_its_full_shape() {
        assert_eq!(Principal::Operator.to_string(), "operator");
        let svc = Principal::Service(ServiceId::parse("trusty-search").unwrap());
        assert_eq!(svc.to_string(), "service:trusty-search");
        assert!(format!("{svc:?}").contains("trusty-search"));
    }

    /// Why: a service id lands in the audit stream, so it is held to the same
    /// grammar as a `CredentialRef` — an unvalidated one would be a second door
    /// for arbitrary text into a retained stream (`C-7.3`).
    /// Test: itself.
    #[test]
    fn service_id_rejects_out_of_grammar_text() {
        assert!(ServiceId::parse("trusty-search").is_ok());
        assert!(ServiceId::parse("Trusty-Search").is_err());
        assert!(ServiceId::parse("trusty_search").is_err());
        assert!(ServiceId::parse("").is_err());
        // A qualified path is a CredentialRef shape, not a service id.
        assert_eq!(
            ServiceId::parse("trusty/search").unwrap_err(),
            CredentialRefError::Shape
        );
    }

    /// Why: pins the asymmetry — a write grant satisfies a read request, never
    /// the reverse. Getting this backwards would silently hand write-capable
    /// credentials to read-only consumers, the exact defect `C-3.10` names.
    /// Test: itself.
    #[test]
    fn scope_read_is_covered_by_write() {
        assert!(Scope::write().covers(&Scope::read()));
        assert!(Scope::write().covers(&Scope::write()));
        assert!(Scope::read().covers(&Scope::read()));
        assert!(!Scope::read().covers(&Scope::write()));
    }

    /// Why: an empty grant scope list must not read as "all scopes" — the
    /// fail-closed reading, matching `C-3.2`'s default-deny posture.
    /// Test: itself.
    #[test]
    fn provider_scopes_must_all_be_covered() {
        let granted = Scope::read().with_provider_scopes(["gmail.readonly", "calendar.readonly"]);
        assert!(granted.covers(&Scope::read().with_provider_scopes(["gmail.readonly"])));
        assert!(granted.covers(&Scope::read()));
        assert!(!granted.covers(&Scope::read().with_provider_scopes(["drive.readonly"])));
        assert!(!Scope::read().covers(&Scope::read().with_provider_scopes(["gmail.readonly"])));
        assert_eq!(
            Scope::read()
                .with_provider_scopes(["gmail.readonly"])
                .to_string(),
            "read[gmail.readonly]"
        );
    }
}
