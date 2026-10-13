//! `extends` chain resolution and the merged owner policy (#8988 PR 1a).
//!
//! Why: an org can share one owner's policy (`duettoresearch` extends
//! `bob-duetto`) without copying it, so the child must merge over its parent.
//! What: [`resolve_chain`] reads `<repos_root>/<owner>/.trusty-mpm-owner.toml`
//! and each parent it `extends`, at most [`MAX_CHAIN`] files, and merges them
//! root-first: `deny` is the union, a child `allow` replaces its parent's, a
//! child `claude_ai_connectors` wins. A cycle, a missing parent, an over-long
//! chain or a bad parent makes the child a bad file.
//! Test: `compose_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::floor::is_floor;
use super::schema::{MAX_CHAIN, OWNER_FILE, OwnerFile, OwnerPolicyError, parse_owner_file};

/// The effective policy of one owner after its chain is merged.
///
/// Test: `a_child_allow_replaces_and_deny_unions`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerPolicy {
    /// User-scope servers that load; `None` = all, empty = none but the floor.
    pub allow: Option<BTreeSet<String>>,
    /// Servers that never load. Applied after `allow` (Q4).
    pub deny: BTreeSet<String>,
    /// `Some(false)` turns the claude.ai connectors off.
    pub claude_ai_connectors: Option<bool>,
}

impl OwnerPolicy {
    /// Merge one file over this policy, as the child of the chain so far.
    fn merge(mut self, file: &OwnerFile) -> Self {
        if let Some(mcp) = &file.mcp {
            if let Some(allow) = &mcp.allow {
                self.allow = Some(allow.iter().cloned().collect());
            }
            self.deny.extend(mcp.deny.iter().flatten().cloned());
            if mcp.claude_ai_connectors.is_some() {
                self.claude_ai_connectors = mcp.claude_ai_connectors;
            }
        }
        self
    }

    /// Re-apply the never-deny floor: drop every floor server from `deny`.
    ///
    /// Why: Q2 — every compose path, the last-good record included, must keep
    /// the floor, and a stored record could predate a floor addition.
    /// Test: `a_last_good_policy_has_the_floor_reapplied`.
    pub fn with_floor(mut self) -> Self {
        self.deny.retain(|name| !is_floor(name));
        self
    }
}

/// A resolved chain: who it names, what it merges to, and its source hash.
///
/// Test: `a_child_allow_replaces_and_deny_unions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChain {
    /// Owners, child first.
    pub chain: Vec<String>,
    /// The merged policy.
    pub policy: OwnerPolicy,
    /// sha256 over every file in the chain, child first.
    pub source_sha256: String,
}

/// Where `owner`'s file lives.
///
/// Test: `a_child_allow_replaces_and_deny_unions`.
pub fn owner_file_path(repos_root: &Path, owner: &str) -> PathBuf {
    repos_root.join(owner).join(OWNER_FILE)
}

/// Resolve `owner`'s file and its `extends` chain.
///
/// Why: see the module doc.
/// What: `Ok(None)` when the owner's own file is absent (the layer is off);
/// `Ok(Some(chain))` for a good chain; `Err` for a bad file anywhere in it.
/// Owner names compare case-insensitively when detecting a cycle.
/// Test: every test in `compose_tests.rs`.
pub fn resolve_chain(
    repos_root: &Path,
    owner: &str,
) -> Result<Option<ResolvedChain>, OwnerPolicyError> {
    let Some((first, bytes)) = parse_owner_file(&owner_file_path(repos_root, owner))? else {
        return Ok(None);
    };
    let mut chain = vec![owner.to_string()];
    let mut files = vec![(first, bytes)];
    while let Some(parent) = files.last().and_then(|(f, _)| f.extends.clone()) {
        if chain.iter().any(|o| o.eq_ignore_ascii_case(&parent)) {
            chain.push(parent);
            return Err(OwnerPolicyError::Cycle { chain });
        }
        if chain.len() == MAX_CHAIN {
            chain.push(parent);
            return Err(OwnerPolicyError::TooDeep { chain });
        }
        let path = owner_file_path(repos_root, &parent);
        match parse_owner_file(&path) {
            Ok(Some(file)) => files.push(file),
            Ok(None) => return Err(OwnerPolicyError::MissingParent { parent, path }),
            Err(source) => {
                return Err(OwnerPolicyError::InvalidParent {
                    parent,
                    source: Box::new(source),
                });
            }
        }
        chain.push(parent);
    }

    let mut hasher = Sha256::new();
    for ((_, bytes), owner) in files.iter().zip(&chain) {
        hasher.update(owner.as_bytes());
        hasher.update([0u8]);
        hasher.update(bytes);
        hasher.update([0u8]);
    }
    let policy = files
        .iter()
        .rev()
        .fold(OwnerPolicy::default(), |acc, (file, _)| acc.merge(file));
    Ok(Some(ResolvedChain {
        chain,
        policy,
        source_sha256: format!("{:x}", hasher.finalize()),
    }))
}

#[cfg(test)]
#[path = "compose_tests.rs"]
mod tests;
