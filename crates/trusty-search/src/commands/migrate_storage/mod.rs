//! Handler for `trusty-search migrate storage` — relocate legacy global-storage
//! indexes into per-project `.trusty-search/` directories (issue #403).
//!
//! Why: prior to #403 indexes store their data in a global platform directory
//! (`<data_dir>/indexes/<id>/`). The new colocated layout puts data at
//! `<root_path>/.trusty-search/` so it travels with the project. This command
//! is the opt-in migration path.
//!
//! Bug fix (issue #491): the old implementation decided whether an index was
//! "already colocated" by reading the `colocated` flag from `indexes.toml`.
//! That flag could be `true` even when:
//!
//!   - `<root>/.trusty-search` was a legacy POINTER FILE (a ~21-28 byte text
//!     file like `index = "itinerator"`), not a directory, AND
//!   - the real data was still in `<data_dir>/indexes/<id>/`.
//!
//! A prior migration set `colocated=true` but its `mkdir` silently failed
//! because the pointer file blocked directory creation. The command then
//! reported "already colocated" and migrated 0 indexes.
//!
//! What: for each entry in `indexes.toml`, classify by ACTUAL filesystem
//! state (not the registry flag), then act accordingly:
//!
//!   - AlreadyColocated — skip (truly done).
//!   - NeedsMigration — create `.trusty-search/`, move data files.
//!   - LegacyPointerFile — remove the pointer FILE first, then migrate.
//!   - SkipDeadRoot — report and skip.
//!   - SkipNoData — report and skip.
//!
//! After moving, update `colocated = true` in `indexes.toml`.
//!
//! Test: `classify::tests` and `migrate::tests` cover every classification
//! variant and migration execution path. The top-level handler is exercised
//! by `handler_tests`.

pub mod classify;
#[cfg(test)]
mod handler_tests;
pub mod migrate;

use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;

use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::{
    indexes_toml_path, load_index_registry, patch_index_registry_entry_at,
};

use classify::{classify_index, IndexMigrationClass};
use migrate::{do_migrate_with_pointer_removal, move_data_files, try_remove_empty_src_dir};

/// Outcome reported per index in the summary table.
///
/// Why: the summary report needs to distinguish a successful move, the
/// various skip reasons, the legacy-pointer-file case, and hard failures.
/// What: enumerates the five terminal states.
/// Test: covered by `handler_tests`.
#[derive(Debug, PartialEq, Eq)]
pub enum MigrateStorageStatus {
    /// Files moved, registry updated.
    Migrated,
    /// Filesystem confirms the index is already in `.trusty-search/` — no-op.
    AlreadyColocated,
    /// `root_path` does not exist on disk — skipped.
    RootMissing,
    /// Neither the colocated dir nor app-data has data — nothing to move.
    NoData,
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
/// colocated storage without a full re-index. Fixes the #491 false
/// "already colocated" report by classifying indexes by filesystem state.
/// What: loads `indexes.toml`, classifies each entry via actual filesystem
/// probes, migrates those that need it, and prints a summary.
/// Test: `cargo run -- migrate storage --dry-run` previews without moving;
/// `handler_tests::handle_legacy_pointer_file_migrates_correctly` exercises
/// the full flow with a LegacyPointerFile fixture.
pub fn handle_migrate_storage(dry_run: bool) -> Result<()> {
    let entries = match load_index_registry() {
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

    let mut migrated_ids: Vec<String> = Vec::new();

    for entry in &entries {
        let id = entry.id.clone();
        let root = entry.root_path.clone();

        // Classify using FILESYSTEM state — never trust the `colocated` flag alone.
        let class = classify_index(&id, &root);

        let result = match class {
            IndexMigrationClass::AlreadyColocated => MigrateStorageResult {
                id: id.clone(),
                root_path: root.clone(),
                status: MigrateStorageStatus::AlreadyColocated,
            },

            IndexMigrationClass::SkipDeadRoot => MigrateStorageResult {
                id: id.clone(),
                root_path: root.clone(),
                status: MigrateStorageStatus::RootMissing,
            },

            IndexMigrationClass::SkipNoData => MigrateStorageResult {
                id: id.clone(),
                root_path: root.clone(),
                status: MigrateStorageStatus::NoData,
            },

            IndexMigrationClass::NeedsMigration { src_dir, dst_dir } => {
                if dry_run {
                    MigrateStorageResult {
                        id: id.clone(),
                        root_path: root.clone(),
                        status: MigrateStorageStatus::Migrated,
                    }
                } else {
                    match move_data_files(&src_dir, &dst_dir, &root) {
                        Ok(_moved) => {
                            try_remove_empty_src_dir(&src_dir);
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
                }
            }

            IndexMigrationClass::LegacyPointerFile {
                pointer_path,
                src_dir,
                dst_dir,
            } => {
                if dry_run {
                    MigrateStorageResult {
                        id: id.clone(),
                        root_path: root.clone(),
                        status: MigrateStorageStatus::Migrated,
                    }
                } else {
                    match do_migrate_with_pointer_removal(&pointer_path, &src_dir, &dst_dir, &root)
                    {
                        Ok(_moved) => {
                            try_remove_empty_src_dir(&src_dir);
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
                }
            }
        };

        print_migrate_line(
            migrated_count + already_count + skipped_count + failed_count + 1,
            total,
            &result,
            dry_run,
        );

        match result.status {
            MigrateStorageStatus::Migrated => {
                migrated_count += 1;
                if !dry_run {
                    migrated_ids.push(id);
                }
            }
            MigrateStorageStatus::AlreadyColocated => already_count += 1,
            MigrateStorageStatus::RootMissing | MigrateStorageStatus::NoData => skipped_count += 1,
            MigrateStorageStatus::Failed(_) => failed_count += 1,
        }
    }

    // Persist the `colocated` flag for the entries this run actually moved.
    //
    // #5344: patch BY ID under the registry write lock, never republish the
    // whole `entries` snapshot. A migration walks and copies data directories,
    // so minutes can pass between the load above and this write while a daemon
    // in another process is registering indexes and flushing timestamps into the
    // same file — a whole-file overwrite deleted all of it and logged success.
    if !dry_run && !migrated_ids.is_empty() {
        match indexes_toml_path() {
            Ok(path) => {
                let mut failed = 0usize;
                for id in &migrated_ids {
                    if let Err(e) =
                        patch_index_registry_entry_at(&path, id, |entry| entry.colocated = true)
                    {
                        eprintln!(
                            "{} could not record colocated=true for '{id}': {e:#}",
                            "✗".red()
                        );
                        failed += 1;
                    }
                }
                tracing::info!(
                    "migrate storage: updated indexes.toml — {} entries now colocated ({} failed)",
                    migrated_ids.len() - failed,
                    failed
                );
            }
            Err(e) => eprintln!(
                "{} could not resolve indexes.toml after migration: {e:#}",
                "✗".red()
            ),
        }
    }

    println!();
    if dry_run {
        println!(
            "{} Dry run: {} would migrate, {} already colocated, {} skipped, {} failed",
            "·".dimmed(),
            migrated_count,
            already_count,
            skipped_count,
            failed_count
        );
    } else {
        println!(
            "{} Migrate storage: {} migrated, {} already colocated, {} skipped, {} failed",
            if failed_count == 0 {
                "✓".green()
            } else {
                "⚠".yellow()
            },
            migrated_count,
            already_count,
            skipped_count,
            failed_count
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
            "(already colocated — filesystem confirmed)".dimmed()
        ),
        MigrateStorageStatus::RootMissing => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "·".dimmed(),
            id.dimmed(),
            format!("(root_path missing: {path})").dimmed()
        ),
        MigrateStorageStatus::NoData => println!(
            "  {} {} {} {}",
            prefix.dimmed(),
            "·".dimmed(),
            id.dimmed(),
            "(no data in app-data or colocated dir — skipped)".dimmed()
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
