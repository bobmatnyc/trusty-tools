//! `secret://` credential refs for org channels, resolved by trusty-secrets
//! (#10423, S1b).
//!
//! Why: #10423 acceptance 2: org credentials use the existing trusty-secrets
//! reference grammar and resolver, never a new grammar. DOC-74 §15.3 (owner
//! ruling 06 R1) lets a ref name only a vault in the caller's own lookup
//! order. The daemon has no checkout, so each org declares that order with
//! its `secrets` field: `<owner>/<repo>` (project vault, then owner vault)
//! or `<owner>` (owner vault only).
//! What: [`parse_in_scope`] parses with `trusty_secrets::SecretRef::parse`
//! and builds the org's [`ScopeSet`]. A pinned ref outside it is refused
//! (`vault_out_of_scope`) before any index or backend read; `secret://KEY`
//! resolves through the lookup order. [`StoreResolver`] reads through
//! `SecretStore::read`, and [`StoreResolver::health`] locates a ref in the
//! names-only index without reading a value. Every error is a fixed code;
//! none carries a value.
//! Test: `send_store_tests.rs` (`resolver_*`, `health_*`, `ref_scope_*`).

use std::sync::Arc;

use trusty_common::credentials::Secret;
use trusty_secrets::store::{KeychainBackend, NamesIndex, ScopeSet, SecretBackend, SecretStore};
use trusty_secrets::{OwnerName, RepoName, SecretRef, SecretsError, VaultName};

use crate::channels::staged_send::SecretResolver;

/// The lookup order an org's `secrets` field declares.
///
/// What: `<owner>/<repo>` is the project vault then the owner vault;
/// `<owner>` is the owner vault alone. Anything else is `invalid_scope`.
/// Test: `ref_scope_accepts_spec_forms_and_refuses_foreign_vaults`.
pub fn org_scopes(scope: &str) -> Result<ScopeSet, &'static str> {
    let mut parts = scope.split('/');
    let owner = parts
        .next()
        .and_then(|o| OwnerName::new(o).ok())
        .ok_or("invalid_scope")?;
    let owner_vault = VaultName::owner(&owner);
    match (parts.next(), parts.next()) {
        (None, _) => Ok(ScopeSet::new(owner_vault, None)),
        (Some(repo), None) => {
            let repo = RepoName::new(repo).map_err(|_| "invalid_scope")?;
            Ok(ScopeSet::new(
                VaultName::project(&owner, &repo),
                Some(owner_vault),
            ))
        }
        _ => Err("invalid_scope"),
    }
}

/// Parse `raw` and check it against the lookup order `scope` declares.
///
/// What: `invalid_ref` when the grammar refuses it; `no_secret_scope` when
/// the org declares no `secrets`; `invalid_scope` for a bad declaration;
/// `vault_out_of_scope` for a pinned vault outside the lookup order. Pure:
/// reads no index and no backend.
/// Test: `ref_scope_accepts_spec_forms_and_refuses_foreign_vaults`.
pub fn parse_in_scope(
    scope: Option<&str>,
    raw: &str,
) -> Result<(SecretRef, ScopeSet), &'static str> {
    let parsed = SecretRef::parse(raw).map_err(|_| "invalid_ref")?;
    let scopes = org_scopes(scope.ok_or("no_secret_scope")?)?;
    if let Some(vault) = parsed.pinned_vault() {
        // #10423: DOC-74 §15.3 ruling 06 R1 — own vaults only.
        scopes
            .require_in_scope(&vault)
            .map_err(|_| "vault_out_of_scope")?;
    }
    Ok((parsed, scopes))
}

/// The health of one credential ref, by name only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefHealth {
    /// The index names the key in a vault of the org's lookup order.
    Present,
    /// The ref does not parse or is outside the org's scope; the code says which.
    Invalid(&'static str),
    /// No vault of the org's lookup order indexes the key.
    Missing,
    /// The store could not answer.
    StoreError(String),
}

/// A [`SecretResolver`] over a trusty-secrets [`SecretStore`].
pub struct StoreResolver {
    store: SecretStore,
}

impl std::fmt::Debug for StoreResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreResolver").finish_non_exhaustive()
    }
}

impl StoreResolver {
    /// The daemon's resolver: the Keychain backend and the default index.
    ///
    /// What: `Err` (a fixed text) when the index location is unknown.
    pub fn production() -> Result<Self, String> {
        let index = NamesIndex::default_location()
            .map_err(|_| "the trusty-secrets index location is unknown".to_owned())?;
        Ok(Self::with(Arc::new(KeychainBackend::new()), index))
    }

    /// A resolver over `backend` and `index` (a test's seam).
    pub fn with(backend: Arc<dyn SecretBackend>, index: NamesIndex) -> Self {
        Self {
            store: SecretStore::new(backend, index),
        }
    }

    /// Whether `raw`, under the org's `scope`, names a key the store
    /// indexes. Reads no value.
    ///
    /// Test: `health_reports_present_missing_and_invalid`.
    pub fn health(&self, scope: Option<&str>, raw: &str) -> RefHealth {
        let (parsed, scopes) = match parse_in_scope(scope, raw) {
            Ok(p) => p,
            Err(code) => return RefHealth::Invalid(code),
        };
        match self.store.locate(&parsed, &scopes) {
            Ok(_) => RefHealth::Present,
            Err(SecretsError::NotFound { .. }) => RefHealth::Missing,
            Err(e) => RefHealth::StoreError(e.to_string()),
        }
    }
}

impl SecretResolver for StoreResolver {
    /// What: [`parse_in_scope`], then `SecretStore::read` over the org's
    /// lookup order. The error is a fixed code or the store's value-free
    /// `Display` text.
    /// Test: `resolver_reads_pinned_and_unpinned_refs`,
    /// `resolver_refuses_out_of_scope_and_invalid_refs`.
    fn resolve(&self, scope: Option<&str>, secret_ref: &str) -> Result<Secret<String>, String> {
        let (parsed, scopes) = parse_in_scope(scope, secret_ref).map_err(str::to_owned)?;
        self.store
            .read(&parsed, &scopes)
            .map(|v| Secret::new(v.expose().to_owned()))
            .map_err(|e| e.to_string())
    }
}
