//! Scope resolution: which project and owner vaults a directory uses.
//!
//! Why: DOC-74 §15.3 — a project vault `trusty/<owner>/<repo>` and an owner
//! vault `trusty/<owner>`, where `<owner>/<repo>` comes from the git remote.
//! There is no machine-wide scope. A `secrets.vault` override replaces the
//! project vault (the only sanctioned way to share one across repositories,
//! §13 Q5); the owner vault still comes from the remote.
//! What: [`ScopeSet`] (project first, then owner) and
//! [`parse_remote_identity`], which reads `<owner>/<repo>` out of a remote
//! URL without ever echoing the URL — a remote URL can carry a token.
//! Test: `scope_remote_url_table`, `scope_derive_reads_the_origin_remote`,
//! `scope_derive_without_a_remote_fails_closed`,
//! `scope_override_replaces_only_the_project_vault`.

use std::path::Path;

use super::platform;
use crate::api::methods::{ScopeInfo, ScopeKind, ScopesResponse};
use crate::api::{OwnerName, RepoName, SecretsError, VaultName};

/// Parse `<owner>/<repo>` out of a git remote URL.
///
/// What: accepts `scheme://[user@]host[:port]/owner/repo` and scp-style
/// `[user@]host:owner/repo`, with or without `.git` and trailing slashes.
/// The path must be exactly two segments, each a valid owner/repository
/// name. Errors are fixed reasons; the URL never appears in them.
/// Test: `scope_remote_url_table`.
pub fn parse_remote_identity(url: &str) -> Result<(OwnerName, RepoName), &'static str> {
    let url = url.trim();
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map(|(_, path)| path),
        None => url.split_once(':').map(|(_, path)| path),
    }
    .ok_or("the origin remote URL has no repository path")?;
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').collect();
    let [owner, repo] = segments.as_slice() else {
        return Err("the origin remote path is not exactly `<owner>/<repo>`");
    };
    let invalid = "the origin remote names an invalid owner or repository";
    let owner = OwnerName::new(owner).map_err(|_| invalid)?;
    let repo = RepoName::new(repo).map_err(|_| invalid)?;
    Ok((owner, repo))
}

/// The vaults a caller resolves against, in order.
///
/// Why: see the module docs.
/// What: a project vault, always present, and an owner vault when the owner
/// is known. Lookup order is project, then owner.
/// Test: `scope_override_replaces_only_the_project_vault`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSet {
    project: VaultName,
    owner: Option<VaultName>,
}

impl ScopeSet {
    /// Build from explicit vaults.
    pub fn new(project: VaultName, owner: Option<VaultName>) -> Self {
        Self { project, owner }
    }

    /// Build from a known identity, honouring a `secrets.vault` override.
    ///
    /// Test: `scope_override_replaces_only_the_project_vault`.
    pub fn from_identity(
        owner: &OwnerName,
        repo: &RepoName,
        vault_override: Option<VaultName>,
    ) -> Self {
        Self {
            project: vault_override.unwrap_or_else(|| VaultName::project(owner, repo)),
            owner: Some(VaultName::owner(owner)),
        }
    }

    /// Derive the scopes for the checkout containing `dir`.
    ///
    /// Why: a wrong vault silently reads or writes another project's secrets,
    /// so a scope that cannot be determined is an error, never a guess.
    /// What: reads the `origin` remote. With no remote, a `secrets.vault`
    /// override still gives a project vault (and no owner vault); with
    /// neither, [`SecretsError::ScopeUndetermined`]. A remote that does not
    /// parse is an error even when an override exists.
    /// Test: `scope_derive_reads_the_origin_remote`,
    /// `scope_derive_without_a_remote_fails_closed`.
    pub fn derive(dir: &Path, vault_override: Option<VaultName>) -> Result<Self, SecretsError> {
        let undetermined = |reason| SecretsError::ScopeUndetermined {
            dir: dir.to_path_buf(),
            reason,
        };
        match platform::origin_remote_url(dir) {
            Some(url) => {
                let (owner, repo) = parse_remote_identity(&url).map_err(undetermined)?;
                Ok(Self::from_identity(&owner, &repo, vault_override))
            }
            None => match vault_override {
                Some(project) => Ok(Self::new(project, None)),
                None => Err(undetermined(
                    "no `origin` git remote and no `secrets.vault` override",
                )),
            },
        }
    }

    /// The project vault.
    pub fn project(&self) -> &VaultName {
        &self.project
    }

    /// The owner vault, when the owner is known.
    pub fn owner(&self) -> Option<&VaultName> {
        self.owner.as_ref()
    }

    /// Vaults in lookup order: project, then owner.
    pub fn lookup_order(&self) -> impl Iterator<Item = &VaultName> {
        std::iter::once(&self.project).chain(self.owner.as_ref())
    }

    /// The `secrets.scopes` response for this set.
    ///
    /// Test: `scope_override_replaces_only_the_project_vault`.
    pub fn to_response(&self) -> ScopesResponse {
        let mut scopes = vec![ScopeInfo {
            kind: ScopeKind::Project,
            vault: self.project.clone(),
        }];
        if let Some(owner) = &self.owner {
            scopes.push(ScopeInfo {
                kind: ScopeKind::Owner,
                vault: owner.clone(),
            });
        }
        ScopesResponse { scopes }
    }
}
