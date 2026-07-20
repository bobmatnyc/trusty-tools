//! Shared owner-only (0600) file permission hardening for on-disk secrets.
//!
//! Why: `tokens.json` and per-profile OAuth client files (issue #3518) both
//! hold credential material and must be unreadable by other local users;
//! duplicating the chmod logic across `storage` and `oauth::client_store`
//! would drift.
//! What: [`restrict_to_owner`] chmods 0600 on Unix; no-op elsewhere (Windows
//! ACLs already default to the owning user for files under the user profile).
//! Test: `storage::tests::save_restricts_permissions_on_unix` and
//! `oauth::client_store::tests::persist_restricts_permissions_on_unix` both
//! exercise this via their callers.

use std::path::Path;

use anyhow::{Context, Result};

/// Restrict `path` to owner read/write only (Unix: mode 0600).
#[cfg(unix)]
pub fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))
}

/// No-op on non-Unix targets.
#[cfg(not(unix))]
pub fn restrict_to_owner(_path: &Path) -> Result<()> {
    Ok(())
}
