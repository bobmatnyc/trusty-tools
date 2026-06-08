//! One-time migration: `indexes.toml` → `allowlist.toml`.
//!
//! Why: the rename from `indexes.toml` to `allowlist.toml` (fixing the macOS
//! path collision) orphans any user who had a real allowlist at the old
//! `config_dir()/trusty-search/indexes.toml` path — a situation that can only
//! arise on Linux (where `config_dir != data_local_dir`).  On macOS the
//! `indexes.toml` at that path IS the daemon registry (it contains `id` and
//! `root_path` fields); importing daemon-registry entries as allowlist
//! approvals would defeat the opt-in security gate entirely, so those entries
//! must be silently skipped.
//!
//! What: `try_migrate_legacy` detects whether a migration is needed (new path
//! absent, old path present), attempts to parse the old file as an
//! `AllowlistConfig`, and — if and only if the file parses cleanly and
//! contains at least one entry — writes `allowlist.toml` and logs a one-line
//! notice. Parse errors (e.g. daemon-registry TOML that no longer round-trips
//! as `AllowlistConfig` after the `root_path` alias was removed) are silently
//! swallowed so macOS hosts boot cleanly.
//!
//! Test: `migration_real_allowlist_is_migrated` and
//! `migration_daemon_registry_is_not_migrated` in `collision_tests.rs`.

use std::path::Path;

use super::AllowlistConfig;

/// Attempt a one-time migration from the legacy `indexes.toml` allowlist path
/// to the new `allowlist.toml` path.
///
/// Why: guards against the rename orphaning Linux users who had a real
/// allowlist at the old path while ensuring daemon-registry entries (macOS) are
/// never silently imported as allowlist approvals.
/// What: no-ops when `new_path` already exists, when the legacy path does not
/// exist, or when the legacy file fails to parse as `AllowlistConfig` (which
/// is the expected outcome on macOS where that path IS the daemon registry).
/// Only writes `new_path` when parsing succeeds and at least one valid entry
/// was found.
/// Test: `migration_real_allowlist_is_migrated`,
/// `migration_daemon_registry_is_not_migrated` in `collision_tests.rs`.
pub fn try_migrate_legacy(new_path: &Path, legacy_path: &Path) {
    // Skip if the new file already exists — migration already ran or user
    // created the file manually.
    if new_path.exists() {
        return;
    }
    // No legacy file → nothing to migrate.
    if !legacy_path.exists() {
        return;
    }
    // Attempt to parse the legacy file as an AllowlistConfig. On macOS the
    // file is the daemon registry (`id` + `root_path` per entry) and will
    // fail to parse now that the `root_path` alias has been removed; we
    // swallow the error and proceed with an empty allowlist.
    let cfg = match AllowlistConfig::load_from(legacy_path) {
        Ok(c) => c,
        Err(_) => {
            // Daemon registry or corrupt file — not a real allowlist.
            return;
        }
    };
    // Don't create an empty allowlist.toml just because the legacy file
    // existed but was empty.
    if cfg.entries.is_empty() {
        return;
    }
    match cfg.save_to(new_path) {
        Ok(()) => {
            tracing::info!(
                "allowlist: migrated {} entr{} from legacy {} to {}",
                cfg.entries.len(),
                if cfg.entries.len() == 1 { "y" } else { "ies" },
                legacy_path.display(),
                new_path.display()
            );
        }
        Err(e) => {
            tracing::warn!(
                "allowlist: migration from {} failed: {e}",
                legacy_path.display()
            );
        }
    }
}

/// Compute the legacy `indexes.toml` path that the allowlist used before the
/// rename.
///
/// Why: both `load_with_legacy_migration` and the migration tests need the
/// same path formula; centralising it avoids drift.
/// What: `config_dir()/trusty-search/indexes.toml`, falling back to a
/// relative path when `config_dir` is unavailable.
/// Test: path derivation validated indirectly by migration tests.
pub fn legacy_allowlist_path() -> std::path::PathBuf {
    match dirs::config_dir() {
        Some(base) => base.join("trusty-search").join("indexes.toml"),
        None => std::path::PathBuf::from("trusty-search-indexes.toml"),
    }
}
