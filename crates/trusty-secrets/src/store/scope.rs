//! Scope resolution: which project and owner vaults a directory uses.
//!
//! Why: DOC-74 §15.3 — a project vault `trusty/<owner>/<repo>` and an owner
//! vault `trusty/<owner>`, where `<owner>/<repo>` comes from the github.com
//! `origin` remote. There is no machine-wide scope. A `secrets.vault`
//! override replaces the project vault (the only sanctioned way to share one
//! across repositories, §13 Q5); the owner vault still comes from the remote.
//! #9328 (owner ruling 06): a tracked override may pick only a vault under
//! the remote's owner (R2), only github.com remotes are accepted (R3), and a
//! caller may name no vault outside its own lookup order (R1).
//! What: [`ScopeSet`] (project first, then owner), [`VaultOverride`], and
//! [`parse_remote_identity`], which reads `<owner>/<repo>` out of a
//! github.com remote URL without ever echoing the URL — a remote URL can
//! carry a token.
//! Test: `scope_remote_url_table`, `scope_derive_reads_the_origin_remote`,
//! `scope_derive_without_a_remote_fails_closed`,
//! `scope_override_replaces_only_the_project_vault`,
//! `scope_non_github_remote_is_refused_with_fixed_text`,
//! `scope_tracked_override_outside_the_owner_is_refused`.

use std::path::Path;

use super::config::MachineSecretsConfig;
use super::platform;
use crate::api::methods::{ScopeInfo, ScopeKind, ScopesResponse};
use crate::api::{OwnerName, RepoName, SecretsError, VaultName};

/// The one remote host 0.1.0 accepts (#9328, owner ruling 06 R3).
pub const SUPPORTED_REMOTE_HOST: &str = "github.com";

/// The fixed reason for a remote on any other host.
const UNSUPPORTED_HOST: &str = "the origin remote is not on github.com";

/// Why [`ScopeSet::require_in_scope`] refuses a pinned vault.
const PINNED_OUT_OF_SCOPE: &str =
    "a pinned vault must be this project's vault or its owner's vault";

/// Why [`ScopeSet::from_identity`] refuses a tracked override.
const TRACKED_OVERRIDE_OUT_OF_SCOPE: &str = "a `secrets.vault` override in the tracked repo file \
     must be `trusty/<owner>/<name>` under the origin remote's owner";

/// Why [`parse_remote_identity`] refused a remote URL.
///
/// What: a fixed reason, never the URL.
/// Test: `scope_remote_url_table`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteRefusal {
    /// The URL is not `<host>/<owner>/<repo>`-shaped.
    Malformed(&'static str),
    /// The host is not github.com.
    // #9328: owner ruling 06 R3.
    UnsupportedHost,
}

impl RemoteRefusal {
    /// The fixed reason text.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed(reason) => reason,
            Self::UnsupportedHost => UNSUPPORTED_HOST,
        }
    }

    fn into_error(self, dir: &Path) -> SecretsError {
        match self {
            Self::Malformed(reason) => SecretsError::ScopeUndetermined {
                dir: dir.to_path_buf(),
                reason,
            },
            Self::UnsupportedHost => SecretsError::UnsupportedRemoteHost {
                dir: dir.to_path_buf(),
            },
        }
    }
}

/// Parse `<owner>/<repo>` out of a github.com remote URL.
///
/// What: accepts `scheme://[user@]github.com[:port]/owner/repo` and
/// scp-style `[user@]github.com:owner/repo`, with or without `.git` and
/// trailing slashes. The host, case-insensitive, must be exactly
/// [`SUPPORTED_REMOTE_HOST`]; any other host is
/// [`RemoteRefusal::UnsupportedHost`]. The path must be exactly two
/// segments, each a valid owner/repository name. Errors are fixed reasons;
/// the URL never appears in them.
/// Test: `scope_remote_url_table`,
/// `scope_non_github_remote_is_refused_with_fixed_text`.
pub fn parse_remote_identity(url: &str) -> Result<(OwnerName, RepoName), RemoteRefusal> {
    let url = url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/'),
        None => url.split_once(':'),
    }
    .ok_or(RemoteRefusal::Malformed(
        "the origin remote URL has no repository path",
    ))?;
    // #9328: the host decides the vault namespace, so `evil.example/acme/app`
    // must never map to `github.com/acme/app`'s vaults (ruling 06 R3). The
    // userinfo, which can carry a token, and the port are dropped unread; an
    // authority holding a URL delimiter is refused, not guessed at.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    if authority.contains(['#', '?', '\\']) || !host.eq_ignore_ascii_case(SUPPORTED_REMOTE_HOST) {
        return Err(RemoteRefusal::UnsupportedHost);
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').collect();
    let [owner, repo] = segments.as_slice() else {
        return Err(RemoteRefusal::Malformed(
            "the origin remote path is not exactly `<owner>/<repo>`",
        ));
    };
    let invalid =
        RemoteRefusal::Malformed("the origin remote names an invalid owner or repository");
    let owner = OwnerName::new(owner).map_err(|_| invalid)?;
    let repo = RepoName::new(repo).map_err(|_| invalid)?;
    Ok((owner, repo))
}

/// A `secrets.vault` override and the file it came from.
///
/// Why: #9328 (owner ruling 06 R2) — the tracked repo file is editable by
/// anyone who can land a change in the repository, so it may only pick a
/// vault under the remote's owner; the untracked machine config is the
/// operator's own and may pick any vault.
/// Test: `scope_tracked_override_outside_the_owner_is_refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultOverride {
    /// From the tracked `.trusty-tools/trusty-secrets.yaml`.
    Tracked(VaultName),
    /// From the untracked machine config's `secrets.project_vaults`.
    Machine(VaultName),
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
    /// What: a [`VaultOverride::Machine`] vault is used as written. A
    /// [`VaultOverride::Tracked`] vault must be `trusty/<owner>/<name>` for
    /// this identity's owner, else [`SecretsError::VaultOutOfScope`] — never
    /// a fallback to the derived vault.
    /// Test: `scope_override_replaces_only_the_project_vault`,
    /// `scope_tracked_override_outside_the_owner_is_refused`.
    pub fn from_identity(
        owner: &OwnerName,
        repo: &RepoName,
        vault_override: Option<VaultOverride>,
    ) -> Result<Self, SecretsError> {
        let project = match vault_override {
            None => VaultName::project(owner, repo),
            Some(VaultOverride::Machine(vault)) => vault,
            // #9328: ruling 06 R2 — a tracked override stays under the owner.
            Some(VaultOverride::Tracked(vault)) if is_project_vault_of(&vault, owner) => vault,
            Some(VaultOverride::Tracked(vault)) => {
                return Err(SecretsError::VaultOutOfScope {
                    vault: vault.to_string(),
                    reason: TRACKED_OVERRIDE_OUT_OF_SCOPE,
                });
            }
        };
        Ok(Self {
            project,
            owner: Some(VaultName::owner(owner)),
        })
    }

    /// Derive the scopes for the checkout containing `dir`.
    ///
    /// Why: a wrong vault silently reads or writes another project's secrets,
    /// so a scope that cannot be determined is an error, never a guess.
    /// What: reads the `origin` remote, which must be on github.com
    /// ([`SecretsError::UnsupportedRemoteHost`]). The override is the
    /// machine config's `project_vaults` entry for this `<owner>/<repo>`
    /// when present, else `tracked` (the repo file's `secrets.vault`),
    /// checked by [`Self::from_identity`]. With no remote there is no owner
    /// to check an override against, so the result is
    /// [`SecretsError::ScopeUndetermined`] whatever the config says.
    /// Test: `scope_derive_reads_the_origin_remote`,
    /// `scope_derive_without_a_remote_fails_closed`,
    /// `scope_non_github_remote_is_refused_with_fixed_text`,
    /// `scope_tracked_override_outside_the_owner_is_refused`.
    pub fn derive(
        dir: &Path,
        tracked: Option<VaultName>,
        machine: Option<&MachineSecretsConfig>,
    ) -> Result<Self, SecretsError> {
        // #9328: no remote, no owner — a tracked override cannot be checked.
        let url = platform::origin_remote_url(dir).ok_or(SecretsError::ScopeUndetermined {
            dir: dir.to_path_buf(),
            reason: "no `origin` git remote",
        })?;
        let (owner, repo) = parse_remote_identity(&url).map_err(|r| r.into_error(dir))?;
        let vault_override = match machine.and_then(|m| m.project_vault(&owner, &repo)) {
            Some(vault) => Some(VaultOverride::Machine(vault.clone())),
            None => tracked.map(VaultOverride::Tracked),
        };
        Self::from_identity(&owner, &repo, vault_override)
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

    /// Refuse a vault outside [`Self::lookup_order`].
    ///
    /// Why: #9328 (owner ruling 06 R1) — a caller reads and writes its own
    /// vaults only; cross-project access waits for the S8 grants.
    /// What: `Ok` for the project or owner vault, else
    /// [`SecretsError::VaultOutOfScope`]. Reads nothing.
    /// Test: `resolve_pinned_reference_outside_the_scopes_is_refused`,
    /// `server_vault_outside_the_project_is_refused`.
    pub fn require_in_scope(&self, vault: &VaultName) -> Result<(), SecretsError> {
        if self.lookup_order().any(|v| v == vault) {
            Ok(())
        } else {
            Err(SecretsError::VaultOutOfScope {
                vault: vault.to_string(),
                reason: PINNED_OUT_OF_SCOPE,
            })
        }
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

/// Whether `vault` is `trusty/<owner>/<name>` for exactly this owner.
///
/// What: the owner vault `trusty/<owner>` itself does not count; both names
/// are already lowercased by validation.
fn is_project_vault_of(vault: &VaultName, owner: &OwnerName) -> bool {
    vault
        .as_str()
        .strip_prefix("trusty/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(vault_owner, _)| vault_owner == owner.as_str())
}
