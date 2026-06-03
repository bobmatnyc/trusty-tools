//! Handler for `trusty-search migrate-redb <path>`.
//!
//! Why: upgrading trusty-search to a release built on redb 4.x (#702 / #707)
//! makes the daemon unable to open any `index.redb` written by the old redb 2.x
//! build. The default auto-recovery moves the stale file aside and recreates an
//! empty corpus, which forces a full reindex — and that reindex RE-EMBEDS every
//! chunk (an ONNX forward pass per chunk), which is the slow part on a large
//! corpus. This subcommand gives operators an explicit, data-preserving path:
//! it copies the old 2.x corpus into a new 4.x corpus verbatim, so the index is
//! preserved and nothing is re-embedded.
//! What: `handle_migrate_redb` resolves the target `index.redb` path, runs
//! [`crate::core::redb_migrate::migrate_redb_corpus`], and prints a human
//! summary (tables copied + row counts, or an already-up-to-date no-op).
//! Test: `cargo run -- migrate-redb <path>` against a 2.x corpus prints the
//! per-table copy summary; the underlying copy logic is unit-tested in
//! `core::redb_migrate::tests`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::core::redb_migrate::{migrate_redb_corpus, MigrationOutcome};

/// Run the `migrate-redb` subcommand against `path`.
///
/// Why: a single entry point the CLI dispatcher calls, keeping clap argument
/// parsing in `main.rs` and the migration orchestration here.
/// What: canonicalises the target path for clearer logging (best-effort — a
/// non-existent canonical parent is tolerated), invokes the copy migration, and
/// renders the outcome. Returns `Ok(())` on success (including the already-4.x
/// no-op) and bubbles a contextual error otherwise so the central dispatcher
/// prints the red-✗ line and picks the exit code.
/// Test: exercised end-to-end by `cargo run -- migrate-redb <fixture>`; the
/// migration itself is covered by `core::redb_migrate::tests`.
pub fn handle_migrate_redb(path: PathBuf) -> Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "no file at {} — pass the path to an index.redb (or its \
             .v2-incompatible backup)",
            path.display()
        );
    }

    println!(
        "🔄 Migrating redb corpus at {} (preserving data, no re-embedding)…\n",
        path.display().to_string().bold()
    );

    let outcome = migrate_redb_corpus(&path)
        .with_context(|| format!("migrate redb corpus at {}", path.display()))?;

    match outcome {
        MigrationOutcome::AlreadyV4 => {
            println!(
                "{} {} already opens with redb 4.x — nothing to migrate.",
                "·".dimmed(),
                path.display()
            );
        }
        MigrationOutcome::Migrated {
            per_table,
            total_rows,
            backup,
            schema_version,
        } => {
            for (name, rows) in &per_table {
                let line = format!("  {name:<22} {rows:>10} rows");
                if *rows > 0 {
                    println!("{} {}", "✓".green(), line);
                } else {
                    println!("{} {}", "·".dimmed(), line.dimmed());
                }
            }
            println!();
            println!(
                "{} Migrated {} rows across {} table(s) → redb 4.x.",
                "✓".green(),
                total_rows.to_string().bold(),
                per_table.iter().filter(|(_, r)| *r > 0).count()
            );
            println!(
                "{} Preserved schema_version = {} (in-app migrations M00x will run normally).",
                "·".dimmed(),
                schema_version
            );
            println!(
                "{} Original 2.x corpus backed up at {}.",
                "·".dimmed(),
                backup.display().to_string().dimmed()
            );
            println!(
                "\n{} Restart the daemon to pick up the migrated index — no reindex needed.",
                "→".cyan()
            );
        }
    }

    Ok(())
}
