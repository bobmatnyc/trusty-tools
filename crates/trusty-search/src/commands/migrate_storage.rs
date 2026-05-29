//! Handler for `trusty-search migrate storage` — relocate legacy global-storage
//! indexes into per-project `.trusty-search/` directories (issue #403).
//!
//! Why: pre-#403 indexes store their data (redb corpus, HNSW snapshot,
//! schema stamp) in a global platform directory (`<data_dir>/indexes/<id>/`).
//! The new colocated layout puts that data at `<root_path>/.trusty-search/`
//! so it travels with the project. This command is the opt-in migration path —
//! it MOVES the existing files (no full re-index required; #402 relative paths
//! make the data relocatable) and updates the registry.
//!
//! What: for each entry in `indexes.toml` that is NOT already `colocated`:
//!   1. Verify `root_path` still exists on disk.
//!   2. Create `<root_path>/.trusty-search/`.
//!   3. Move every file from `<data_dir>/indexes/<id>/` to `.trusty-search/`.
//!   4. Update `indexes.toml` entry: set `colocated = true`.
//!   5. Register `root_path` in `roots.toml`.
//!   6. Add `.gitignore` entry.
//!   7. Delete the now-empty global `indexes/<id>/` directory.
//!
//! Test: `migrate_storage_moves_files_and_updates_registry` verifies that
//! files move and the registry entry is updated without a re-index.

use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;

use crate::service::colocated_storage::{
    colocated_storage_dir, ensure_gitignored, COLOCATED_DIR_NAME,
};
use crate::service::persistence::{index_data_dir, load_index_registry, save_index_registry};
use crate::service::roots_registry::upsert_root;

/// Outcome of attempting to migrate one legacy index.
///
/// Why: the summary report needs to distinguish a successful move, a skip
/// (already colocated), a skip (root_path gone), and a hard failure.
/// What: enumerates the four terminal states.
/// Test: covered by `migrate_storage_moves_files_and_updates_registry`.
#[derive(Debug, PartialEq, Eq)]
pub enum MigrateStorageStatus {
    /// Files moved, registry updated.
    Migrated,
    /// Entry already had `colocated = true` — no-op.
    AlreadyColocated,
    /// `root_path` does not exist on disk — skipped (index may be for a
    /// removed or unmounted project).
    RootMissing,
    /// An error occurred during the move.
    Failed(String),
}

/// Result of migrating one index.
#[derive(Debug)]
pub struct MigrateStorageResult {
    pub id: String,
    pub root_path: PathBuf,
    pub status: MigrateStorageStatus,
}

/// Entry point for `trusty-search migrate storage`.
///
/// Why: gives operators a single command to opt all legacy indexes into
/// colocated storage without a full re-index.
/// What: loads `indexes.toml`, migrates each non-colocated entry, and
/// prints a summary.
/// Test: `cargo run -- migrate storage --dry-run` previews without moving.
pub fn handle_migrate_storage(dry_run: bool) -> Result<()> {
    let mut entries = match load_index_registry() {
        Ok(e) => e,
        Err(e) => anyhow::bail!("could not read indexes.toml: {e}"),
    };

    if entries.is_empty() {
        println!(
            "{} No indexes registered — nothing to migrate.",
            "·".dimmed()
        );
        return Ok(());
    }

    if dry_run {
        println!(
            "{} Dry run — no files or registry entries will be modified.\n",
            "·".dimmed()
        );
    }

    let total = entries.len();
    let mut migrated_count = 0usize;
    let mut already_count = 0usize;
    let mut skipped_count = 0usize;
    let mut failed_count = 0usize;

    for entry in &mut entries {
        let id = entry.id.clone();
        let root = entry.root_path.clone();

        let result = if entry.colocated {
            MigrateStorageResult {
                id: id.clone(),
                root_path: root.clone(),
                status: MigrateStorageStatus::AlreadyColocated,
            }
        } else if !root.exists() || !root.is_dir() {
            MigrateStorageResult {
                id: id.clone(),
                root_path: root.clone(),
                status: MigrateStorageStatus::RootMissing,
            }
        } else if dry_run {
            // Dry run: report what would happen without moving.
            MigrateStorageResult {
                id: id.clone(),
                root_path: root.clone(),
                status: MigrateStorageStatus::Migrated,
            }
        } else {
            match migrate_one_index(&id, &root) {
                Ok(()) => {
                    // Update the in-memory entry to mark it colocated.
                    entry.colocated = true;
                    MigrateStorageResult {
                        id: id.clone(),
                        root_path: root.clone(),
                        status: MigrateStorageStatus::Migrated,
                    }
                }
                Err(e) => MigrateStorageResult {
                    id: id.clone(),
                    root_path: root.clone(),
                    status: MigrateStorageStatus::Failed(format!("{e:#}")),
                },
            }
        };

        print_migrate_line(
            migrated_count + already_count + skipped_count + failed_count + 1,
            total,
            &result,
            dry_run,
        );

        match result.status {
            MigrateStorageStatus::Migrated => migrated_count += 1,
            MigrateStorageStatus::AlreadyColocated => already_count += 1,
            MigrateStorageStatus::RootMissing => skipped_count += 1,
            MigrateStorageStatus::Failed(_) => failed_count += 1,
        }
    }

    // Persist updated registry (only when not dry-run and at least one was migrated).
    if !dry_run && migrated_count > 0 {
        if let Err(e) = save_index_registry(&entries) {
            eprintln!(
                "{} could not save indexes.toml after migration: {e:#}",
                "✗".red()
            );
        } else {
            tracing::info!(
                "migrate storage: updated indexes.toml — {} entries now colocated",
                migrated_count
            );
        }
    }

    println!();
    if dry_run {
        println!(
            "{} Dry run: {} would migrate, {} already colocated, {} skipped (root missing), {} failed",
            "·".dimmed(), migrated_count, already_count, skipped_count, failed_count
        );
    } else {
        println!(
            "{} Migrate storage: {} migrated, {} already colocated, {} skipped (root missing), {} failed",
            if failed_count == 0 { "✓".green() } else { "⚠".yellow() },
            migrated_count, already_count, skipped_count, failed_count
        );
        if migrated_count > 0 {
            println!(
                "\n{} Restart the daemon to use the new colocated paths:\n  trusty-search stop && trusty-search start",
                "ℹ".cyan()
            );
        }
    }

    Ok(())
}

/// Move a single legacy index from global storage to colocated storage.
///
/// Why: the actual file-move logic is separated so it can be unit-tested
/// without the CLI machinery.
/// What:
///   1. Resolve the source dir: `<data_dir>/indexes/<id>/`.
///   2. Create `<root_path>/.trusty-search/`.
///   3. Move every file in the source dir to the dest dir.
///   4. Attempt to remove the (now-empty) source dir.
///   5. Register root in `roots.toml` and add `.gitignore`.
///
/// Test: `migrate_storage_moves_files_and_updates_registry`.
fn migrate_one_index(index_id: &str, root_path: &std::path::Path) -> Result<()> {
    use anyhow::Context;

    let src_dir = index_data_dir(index_id).context("resolve legacy index data dir")?;
    let dst_dir = colocated_storage_dir(root_path).context("create colocated storage dir")?;

    // Move every file from src to dst.
    let read_dir = std::fs::read_dir(&src_dir)
        .with_context(|| format!("read legacy index dir {}", src_dir.display()))?;

    let mut moved = 0usize;
    for entry in read_dir.flatten() {
        let from = entry.path();
        let file_name = match from.file_name() {
            Some(n) => n,
            None => continue,
        };
        let to = dst_dir.join(file_name);
        // Attempt rename first (same filesystem, cheap). Fall back to copy+delete
        // across filesystem boundaries.
        if std::fs::rename(&from, &to).is_err() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} → {}", from.display(), to.display()))?;
            // Ignore delete errors — the source will be stale but the data is safe.
            let _ = std::fs::remove_file(&from);
        }
        moved += 1;
    }

    tracing::info!(
        "migrate storage: moved {moved} file(s) from {} → {}",
        src_dir.display(),
        dst_dir.display()
    );

    // Remove the now-empty source dir. Non-fatal if it fails (may have
    // leftover files we couldn't move, or may already be gone).
    if let Err(e) = std::fs::remove_dir(&src_dir) {
        tracing::debug!(
            "migrate storage: could not remove source dir {} ({e}) — non-fatal",
            src_dir.display()
        );
    }

    // Register root and add .gitignore.
    upsert_root(root_path.to_path_buf()).context("register root in roots.toml")?;
    if let Err(e) = ensure_gitignored(root_path) {
        tracing::warn!(
            "migrate storage: could not add .gitignore entry for {}: {e}",
            root_path.display()
        );
    }

    Ok(())
}

/// Render one result line for the summary table.
fn print_migrate_line(idx: usize, total: usize, r: &MigrateStorageResult, dry_run: bool) {
    let prefix = format!("[{idx}/{total}]");
    let id = &r.id;
    let path = r.root_path.display().to_string();
    match &r.status {
        MigrateStorageStatus::Migrated => {
            if dry_run {
                println!(
                    "  {} {} {} {}",
                    prefix.dimmed(),
                    "→".cyan(),
                    id.bold(),
                    format!("(would move to {}/.trusty-search/)", path).dimmed()
                );
            } else {
                println!(
                    "  {} {} {} → {}{}",
                    prefix.dimmed(),
                    "✓".green(),
                    id.bold(),
                    path.dimmed(),
                    format!("/{COLOCATED_DIR_NAME}/").dimmed()
                );
            }
        }
        MigrateStorageStatus::AlreadyColocated => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "↻".cyan(),
            id.dimmed(),
            "(already colocated)".dimmed()
        ),
        MigrateStorageStatus::RootMissing => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "·".dimmed(),
            id.dimmed(),
            format!("(root_path missing: {path})").dimmed()
        ),
        MigrateStorageStatus::Failed(msg) => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "✗".red(),
            id.dimmed(),
            format!("({msg})").red()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::persistence::{data_dir, save_index_registry, PersistedIndex};
    use serial_test::serial;
    use tempfile::tempdir;

    /// Why: the migration must physically move files from the legacy dir to the
    /// colocated dir WITHOUT requiring a re-index.
    /// What: create a fake legacy index dir with sentinel files, run the
    /// migration, assert files have moved and the colocated dir exists.
    /// Test: `migrate_storage_moves_files_and_updates_registry` (this test).
    ///
    /// `#[serial]` is required because the test mutates the `TRUSTY_DATA_DIR`
    /// process-level environment variable, which is shared across all threads
    /// in the test binary. Running these tests concurrently causes races where
    /// one test's `set_var` clobbers another test's directory, producing
    /// spurious "rename indexes.toml / No such file or directory" panics.
    #[test]
    #[serial]
    fn migrate_storage_moves_files_and_updates_registry() {
        // Create an isolated data dir so we don't touch the real one.
        let data_dir_tmp = tempdir().unwrap();
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
        }

        // Create a fake project root with something inside it (simulates git repo).
        let project_root = tempdir().unwrap();
        let index_id = "test-migrate-idx";

        // Populate the legacy index data dir with sentinel files.
        let legacy_dir = data_dir().unwrap().join("indexes").join(index_id);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("index.redb"), b"redb-sentinel").unwrap();
        std::fs::write(legacy_dir.join("hnsw.usearch"), b"hnsw-sentinel").unwrap();
        std::fs::write(legacy_dir.join("schema_version.json"), b"{}").unwrap();

        // Write an indexes.toml with the legacy entry.
        let registry_path = data_dir().unwrap().join("indexes.toml");
        save_index_registry(&[PersistedIndex {
            id: index_id.to_string(),
            root_path: project_root.path().to_path_buf(),
            colocated: false,
            ..Default::default()
        }])
        .unwrap();

        // Run the migration.
        migrate_one_index(index_id, project_root.path()).unwrap();

        // Files must be in the colocated dir.
        let colocated = project_root.path().join(".trusty-search");
        assert!(
            colocated.exists(),
            "colocated dir must exist after migration"
        );
        assert!(
            colocated.join("index.redb").exists(),
            "index.redb must be in colocated dir"
        );
        assert!(
            colocated.join("hnsw.usearch").exists(),
            "hnsw.usearch must be in colocated dir"
        );
        assert!(
            colocated.join("schema_version.json").exists(),
            "schema_version.json must be in colocated dir"
        );

        // Legacy dir must be gone or empty.
        assert!(
            !legacy_dir.exists() || std::fs::read_dir(&legacy_dir).unwrap().next().is_none(),
            "legacy dir must be empty or deleted after migration"
        );

        // roots.toml must contain the root.
        let roots = crate::service::roots_registry::load_roots().unwrap();
        assert!(
            roots.iter().any(|r| r.path == project_root.path()),
            "root must be registered in roots.toml after migration"
        );

        // .gitignore must contain .trusty-search/.
        let gitignore_content =
            std::fs::read_to_string(project_root.path().join(".gitignore")).unwrap_or_default();
        assert!(
            gitignore_content.contains(".trusty-search/"),
            ".gitignore must contain .trusty-search/ after migration"
        );

        // Check that file contents survived the move.
        assert_eq!(
            std::fs::read(colocated.join("index.redb")).unwrap(),
            b"redb-sentinel"
        );

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        // Ensure the registry path var is not set after cleanup (registry lives in the temp data dir)
        let _ = registry_path;
    }

    /// Why: migrating an index whose root_path no longer exists must be a
    /// non-error skip rather than a panic.
    /// What: register a legacy index with a non-existent root; run the
    /// full CLI handler; assert the result is `RootMissing`.
    /// Test: `migrate_storage_skips_missing_root`.
    ///
    /// `#[serial]` — see `migrate_storage_moves_files_and_updates_registry`.
    #[test]
    #[serial]
    fn migrate_storage_skips_missing_root() {
        let data_dir_tmp = tempdir().unwrap();
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
        }

        let nonexistent_root = PathBuf::from("/tmp/trusty-test-missing-root-12345");
        save_index_registry(&[PersistedIndex {
            id: "gone-index".to_string(),
            root_path: nonexistent_root.clone(),
            colocated: false,
            ..Default::default()
        }])
        .unwrap();

        // Run the top-level handler in dry-run=false; it should not panic.
        handle_migrate_storage(false).unwrap();

        // The registry entry must still be non-colocated (nothing moved).
        let entries = load_index_registry().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            !entries[0].colocated,
            "missing-root index must not be marked colocated"
        );

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }
    }

    /// Why: running the command on an already-colocated index must be a
    /// no-op — the `AlreadyColocated` status must be returned.
    ///
    /// `#[serial]` — see `migrate_storage_moves_files_and_updates_registry`.
    #[test]
    #[serial]
    fn migrate_storage_idempotent_for_colocated() {
        let data_dir_tmp = tempdir().unwrap();
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
        }

        let project_root = tempdir().unwrap();
        save_index_registry(&[PersistedIndex {
            id: "col-index".to_string(),
            root_path: project_root.path().to_path_buf(),
            colocated: true,
            ..Default::default()
        }])
        .unwrap();

        handle_migrate_storage(false).unwrap();

        let entries = load_index_registry().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].colocated,
            "already-colocated must remain colocated"
        );

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }
    }
}
