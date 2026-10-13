//! The last good owner policy on record (#8988 PR 1a).
//!
//! Why: a bad owner file must never refuse a launch or resume (owner ruling 4),
//! and failing open to "no policy" would silently re-load every server the
//! operator denied. The last policy that parsed is the middle ground.
//! What: one JSON record per owner at
//! `<state_root>/owner-policy/<owner>.last-good.json` (state root
//! `~/.trusty-tools/trusty-mpm`), mode `0600`, written atomically by
//! [`record_last_good`] and read by [`read_last_good`], which re-applies the
//! never-deny floor. Only a GOOD chain is ever written.
//! Test: `last_good_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::compose::{OwnerPolicy, ResolvedChain};
use super::schema::is_owner_name;

/// Directory under the state root that holds every record.
pub const LAST_GOOD_DIR: &str = "owner-policy";

/// Record format version.
const VERSION: u32 = 1;

/// One stored last-good policy.
///
/// Test: `a_recorded_policy_reads_back_with_its_timestamp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastGood {
    /// Record format version.
    pub version: u32,
    /// The owner the record belongs to.
    pub owner: String,
    /// The `extends` chain it was resolved from, child first.
    pub chain: Vec<String>,
    /// The merged policy.
    pub policy: OwnerPolicy,
    /// sha256 of the chain's source files.
    pub source_sha256: String,
    /// When it was recorded, RFC 3339 UTC.
    pub recorded_at: String,
}

/// Where `owner`'s record lives under `state_root`.
///
/// What: owner names are case-insensitive, so the file name is lowercased.
/// Test: `a_recorded_policy_reads_back_with_its_timestamp`.
pub fn last_good_path(state_root: &Path, owner: &str) -> PathBuf {
    state_root
        .join(LAST_GOOD_DIR)
        .join(format!("{}.last-good.json", owner.to_ascii_lowercase()))
}

/// Read `owner`'s last-good record.
///
/// What: `Ok(None)` when absent; `Err(reason)` when unreadable, unparsable, of
/// another version, or naming another owner; otherwise the record with the
/// floor re-applied to its policy (Q2).
/// Test: `an_absent_record_is_none`, `a_corrupt_record_is_an_error`,
/// `a_last_good_policy_has_the_floor_reapplied`.
pub fn read_last_good(state_root: &Path, owner: &str) -> Result<Option<LastGood>, String> {
    let path = last_good_path(state_root, owner);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    let record: LastGood = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a valid last-good record: {e}", path.display()))?;
    if record.version != VERSION || !record.owner.eq_ignore_ascii_case(owner) {
        return Err(format!(
            "{} is version {} for owner `{}`, not version {VERSION} for `{owner}`",
            path.display(),
            record.version,
            record.owner
        ));
    }
    Ok(Some(LastGood {
        policy: record.policy.with_floor(),
        ..record
    }))
}

/// Store a good chain as `owner`'s last-good record.
///
/// Why: the fallback can only use what was recorded while the file was good.
/// What: a no-op when the stored record already carries the same source sha;
/// otherwise writes the record atomically, mode `0600` in a `0700` directory.
/// Test: `a_recorded_policy_reads_back_with_its_timestamp`,
/// `recording_the_same_source_twice_keeps_the_first_timestamp`,
/// `the_record_is_owner_only`.
pub fn record_last_good(
    state_root: &Path,
    owner: &str,
    resolved: &ResolvedChain,
) -> std::io::Result<()> {
    if !is_owner_name(owner) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("`{owner}` is not an owner name"),
        ));
    }
    if let Ok(Some(existing)) = read_last_good(state_root, owner)
        && existing.source_sha256 == resolved.source_sha256
    {
        return Ok(());
    }
    let record = LastGood {
        version: VERSION,
        owner: owner.to_string(),
        chain: resolved.chain.clone(),
        policy: resolved.policy.clone(),
        source_sha256: resolved.source_sha256.clone(),
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let bytes = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
    let path = last_good_path(state_root, owner);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    // `write_atomic` carries the target's mode over, so a new target is
    // created owner-only first.
    #[cfg(unix)]
    if !path.exists() {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
    }
    trusty_common::atomic_file::write_atomic(&path, &bytes)
}

#[cfg(test)]
#[path = "last_good_tests.rs"]
mod tests;
