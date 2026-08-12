//! One-shot, idempotent on-disk migrations applied at daemon startup.
//!
//! Why: Some legacy palace metadata predates a presentation-only change we want
//! to apply uniformly to every existing install. The default unscoped palace
//! ships with id `"localLLM"` and historically used the same string as its
//! display `name`; we want the human-facing label to read `"User Memories"`
//! while keeping `PalaceId` (and therefore the on-disk directory layout)
//! stable. Issue #98 narrowed the scope explicitly to this rename — no public
//! rename API, no HTTP endpoint, no MCP tool — so the migration is the
//! smallest possible surface: a private helper that touches `palace.json`
//! directly and is invoked exactly once per boot.
//! What: `migrate_default_palace_name` walks the registry root for the
//! `localLLM` palace, and only when its persisted `name` is still the literal
//! string `"localLLM"` rewrites the metadata via
//! `PalaceStore::save_palace` with `name = "User Memories"`. Running the
//! migration twice is a no-op; running it against a palace already renamed by
//! the user is also a no-op.
//! Test: see the module-level `tests` block — covers the rename, idempotency,
//! and the negative case where the palace has already been renamed.

use anyhow::{Context, Result};
use std::path::Path;
use trusty_common::memory_core::store::{PalaceStore, PalaceStoreError};

/// Stable on-disk identifier for the default unscoped palace.
const DEFAULT_PALACE_ID: &str = "localLLM";
/// The legacy display name we want to migrate away from.
const LEGACY_DEFAULT_NAME: &str = "localLLM";
/// The new human-facing display name.
const NEW_DEFAULT_NAME: &str = "User Memories";

/// Rename the default `localLLM` palace's display `name` to "User Memories"
/// when (and only when) its persisted name is still the legacy literal.
///
/// Why: Issue #98 — refresh the default palace's display label without
/// touching its `PalaceId`, on-disk directory, or any caller-visible API.
/// What: Reads `<registry_root>/localLLM/palace.json` via `PalaceStore`. If
/// the file is genuinely absent, or the `name` field already differs from the
/// legacy literal, the function is a no-op. Otherwise it rewrites
/// `palace.json` atomically with the new name. Metadata whose presence cannot
/// be determined is an error rather than a silent no-op (#5549, ADR-0045) —
/// startup surfaces it once instead of skipping the rename every boot.
/// Test: `tests::migrates_when_legacy_name`,
/// `tests::idempotent_when_already_renamed`,
/// `tests::leaves_custom_name_untouched`,
/// `tests::missing_palace_is_noop`,
/// `tests::unstattable_palace_json_is_an_error_not_a_noop`.
///
/// This is an internal migration entrypoint, not a general-purpose rename
/// API. It is `pub` only so the `trusty-memory` binary crate can invoke it
/// from `main.rs`; do not call it from other crates.
pub fn migrate_default_palace_name(registry_root: &Path) -> Result<()> {
    let palace_dir = registry_root.join(DEFAULT_PALACE_ID);
    // #5549: the pre-check was `palace.json.exists()`, which read a denied stat
    // as "no default palace here" and skipped the migration without a word.
    let mut palace = match PalaceStore::load_palace(&palace_dir) {
        Ok(p) => p,
        // No default palace on this host — nothing to do.
        Err(PalaceStoreError::NotFound(_)) => return Ok(()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("load palace metadata at {}", palace_dir.display()))
        }
    };

    if palace.name != LEGACY_DEFAULT_NAME {
        // Already renamed (either by an earlier migration run or by the user
        // editing palace.json directly). Leave it alone.
        return Ok(());
    }

    palace.name = NEW_DEFAULT_NAME.to_string();
    PalaceStore::save_palace(&palace)
        .with_context(|| format!("rewrite palace metadata at {}", palace_dir.display()))?;
    tracing::info!(
        palace_id = DEFAULT_PALACE_ID,
        old_name = LEGACY_DEFAULT_NAME,
        new_name = NEW_DEFAULT_NAME,
        "migrated default palace display name"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::tempdir;
    use trusty_common::memory_core::palace::{Palace, PalaceId};

    /// Build a `Palace` with the given id + name and persist it under
    /// `<registry_root>/<id>/palace.json`. Returns the persisted palace.
    fn persist_palace(registry_root: &Path, id: &str, name: &str) -> Palace {
        let data_dir = registry_root.join(id);
        std::fs::create_dir_all(&data_dir).expect("create palace data dir");
        let palace = Palace {
            id: PalaceId::new(id),
            name: name.to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_dir.clone(),
        };
        PalaceStore::save_palace(&palace).expect("persist palace metadata");
        palace
    }

    #[test]
    fn migrates_when_legacy_name() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        persist_palace(root, DEFAULT_PALACE_ID, LEGACY_DEFAULT_NAME);

        migrate_default_palace_name(root).expect("migration runs");

        let loaded =
            PalaceStore::load_palace(&root.join(DEFAULT_PALACE_ID)).expect("reload palace.json");
        assert_eq!(loaded.id.as_str(), DEFAULT_PALACE_ID, "id must be stable");
        assert_eq!(
            loaded.name, NEW_DEFAULT_NAME,
            "display name must be updated"
        );
    }

    #[test]
    fn idempotent_when_already_renamed() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        persist_palace(root, DEFAULT_PALACE_ID, LEGACY_DEFAULT_NAME);

        // First pass renames; second pass must be a no-op.
        migrate_default_palace_name(root).expect("first migration");
        migrate_default_palace_name(root).expect("second migration is no-op");

        let loaded =
            PalaceStore::load_palace(&root.join(DEFAULT_PALACE_ID)).expect("reload palace.json");
        assert_eq!(loaded.name, NEW_DEFAULT_NAME);
    }

    #[test]
    fn leaves_custom_name_untouched() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // User-chosen name that does NOT match the legacy literal — migration
        // must leave it alone even though the id matches.
        persist_palace(root, DEFAULT_PALACE_ID, "My Custom Memories");

        migrate_default_palace_name(root).expect("migration runs");

        let loaded =
            PalaceStore::load_palace(&root.join(DEFAULT_PALACE_ID)).expect("reload palace.json");
        assert_eq!(
            loaded.name, "My Custom Memories",
            "custom names must not be overwritten"
        );
    }

    #[test]
    fn pre_renamed_user_memories_is_untouched() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // Someone (a prior migration in another worktree, a manual edit) has
        // already set the name to "User Memories". The migration must still
        // be a no-op rather than re-saving the file.
        persist_palace(root, DEFAULT_PALACE_ID, NEW_DEFAULT_NAME);

        migrate_default_palace_name(root).expect("migration runs");

        let loaded =
            PalaceStore::load_palace(&root.join(DEFAULT_PALACE_ID)).expect("reload palace.json");
        assert_eq!(loaded.name, NEW_DEFAULT_NAME);
    }

    #[test]
    fn missing_palace_is_noop() {
        let tmp = tempdir().unwrap();
        // No palace at all — migration must succeed without error.
        migrate_default_palace_name(tmp.path()).expect("missing palace is a no-op");
    }

    #[test]
    fn unrelated_palaces_are_not_touched() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // Another palace with a different id that happens to share the legacy
        // name string — migration must NOT touch it because the id is the
        // selector.
        persist_palace(root, "other-palace", LEGACY_DEFAULT_NAME);

        migrate_default_palace_name(root).expect("migration runs");

        let loaded =
            PalaceStore::load_palace(&root.join("other-palace")).expect("reload other palace");
        assert_eq!(
            loaded.name, LEGACY_DEFAULT_NAME,
            "non-default palaces must not be touched"
        );
    }

    /// Restore a directory's mode on drop, including while unwinding, so a
    /// failed assertion cannot leave `tempfile` an undeletable tree.
    #[cfg(unix)]
    struct RestoreMode(std::path::PathBuf);

    #[cfg(unix)]
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    /// Why (#5549): the guard here was `palace.json.exists()`, sitting directly
    /// in front of the `load_palace` this change hardened. A denied stat read
    /// as "no default palace on this host", so the migration no-op'd every boot
    /// and never reached the propagation one layer down.
    /// What: locks the palace directory to mode 000 so stat of its metadata is
    /// denied, and asserts the migration errors instead of returning `Ok(())`.
    /// Panics rather than passing vacuously if the denial does not take hold.
    /// Test: This test itself.
    #[cfg(unix)]
    #[test]
    fn unstattable_palace_json_is_an_error_not_a_noop() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let root = tmp.path();
        persist_palace(root, DEFAULT_PALACE_ID, LEGACY_DEFAULT_NAME);
        let palace_dir = root.join(DEFAULT_PALACE_ID);

        std::fs::set_permissions(&palace_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _restore = RestoreMode(palace_dir.clone());

        let target = palace_dir.join("palace.json");
        match std::fs::metadata(&target) {
            Ok(_) => panic!(
                "cannot exercise #5549: stat of {} still succeeds with its parent at mode 000. \
                 Run this suite as a non-root user on a filesystem that honours POSIX \
                 permission bits.",
                target.display()
            ),
            Err(e) => assert_eq!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied,
                "expected the locked palace dir to deny stat of its metadata, got {e}"
            ),
        }

        migrate_default_palace_name(root)
            .expect_err("a palace.json that cannot be stat'd must not read as no palace at all");
    }
}
