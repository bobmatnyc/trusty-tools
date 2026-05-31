//! Handler for `trusty-search prune-orphans` (issue #489).
//!
//! Why: over time, the `indexes.toml` registry accumulates entries whose
//! `root_path` no longer exists — projects deleted from disk, wiped volumes
//! (e.g. `/Volumes/Kemono`), or `/tmp` test indexes. These orphaned entries
//! clutter startup logs ("no durable corpus"), waste registry space, and
//! are never reachable by a reindex. A dedicated offline command lets operators
//! batch-remove them without needing the daemon to be running.
//!
//! What: loads `indexes.toml`, identifies entries whose `root_path` does not
//! exist on disk, prints the list, and (unless `--dry-run`) asks for
//! confirmation before removing them via the existing atomic save path.
//! `--dry-run` previews the list and exits with no mutations.
//! Works entirely OFFLINE — no daemon connection required.
//!
//! Test: `prune_orphans_removes_dead_root_entries`,
//! `prune_orphans_preserves_live_root_entries`,
//! `prune_orphans_dry_run_mutates_nothing`.

use anyhow::Result;
use colored::Colorize;
use std::io::{BufRead, Write};

use crate::service::persistence::{
    indexes_toml_path, load_index_registry_at, save_index_registry_at, PersistedIndex,
};

/// Why: a small record per orphaned entry keeps the display and removal
/// logic independent of the full `PersistedIndex` shape.
/// What: holds the id and the dead root path (for display only).
/// Test: covered transitively by `handle_prune_orphans`.
struct OrphanEntry {
    id: String,
    root_path: String,
}

/// Handle `trusty-search prune-orphans [--dry-run] [--yes]`.
///
/// Why: extracted so `main()` stays thin and this function is independently
/// testable with path-injectable helpers.
/// What: loads the registry, filters to entries whose `root_path` does not
/// exist, prints the table, prompts for confirmation (unless `--yes` or
/// `--dry-run`), then removes the orphans from disk via `save_index_registry_at`.
/// `--dry-run` overrides `--yes` — it never mutates the registry.
/// Test: `cargo run -p trusty-search -- prune-orphans --dry-run` prints the
/// table and exits 0 without modifying `indexes.toml`.
pub fn handle_prune_orphans(dry_run: bool, yes: bool) -> Result<()> {
    let toml_path = indexes_toml_path()?;
    handle_prune_orphans_at(&toml_path, dry_run, yes, /*interactive=*/ true)
}

/// Path-injectable variant of [`handle_prune_orphans`].
///
/// Why: tests need to drive this against a tempfile registry without touching
/// the user's real `~/Library/Application Support/trusty-search/indexes.toml`.
/// What: same logic as `handle_prune_orphans`, but reads from and writes to
/// `toml_path` instead of the platform default. `interactive` must be `false`
/// in tests so the stdin-prompt branch is never hit.
/// Test: all `prune_orphans_*` unit tests call this variant.
pub(crate) fn handle_prune_orphans_at(
    toml_path: &std::path::Path,
    dry_run: bool,
    yes: bool,
    interactive: bool,
) -> Result<()> {
    // 1. Load the registry.
    let entries = load_index_registry_at(toml_path)?;

    if entries.is_empty() {
        println!("Registry is empty — nothing to prune.");
        return Ok(());
    }

    // 2. Classify: orphaned (root_path missing) vs. live.
    let (orphans, live): (Vec<PersistedIndex>, Vec<PersistedIndex>) =
        entries.into_iter().partition(|e| !e.root_path.exists());

    if orphans.is_empty() {
        println!(
            "{} All {} registered index(es) have live root paths — nothing to prune.",
            "✓".green(),
            live.len()
        );
        return Ok(());
    }

    let orphan_records: Vec<OrphanEntry> = orphans
        .iter()
        .map(|e| OrphanEntry {
            id: e.id.clone(),
            root_path: e.root_path.display().to_string(),
        })
        .collect();

    // 3. Print the table.
    let count = orphan_records.len();
    println!(
        "{} {} orphaned index registration(s) (root_path missing):",
        "Found".bold(),
        count.to_string().bold()
    );
    let name_width = orphan_records
        .iter()
        .map(|e| e.id.len())
        .max()
        .unwrap_or(0)
        .max(4);
    for e in &orphan_records {
        println!(
            "  {:<width$}  {}",
            e.id.bold(),
            e.root_path.dimmed(),
            width = name_width
        );
    }

    // 4. Dry-run: stop here, no mutations.
    if dry_run {
        println!(
            "{} dry-run: {} registration(s) would be removed. Re-run without --dry-run to apply.",
            "ℹ".cyan(),
            count
        );
        return Ok(());
    }

    // 5. Prompt unless --yes or non-interactive.
    if !yes {
        if !interactive {
            // Non-interactive (tests): treat as cancelled.
            println!("Aborted (non-interactive mode).");
            return Ok(());
        }
        if !confirm(&format!(
            "Remove {} orphaned registration(s) from indexes.toml?",
            count
        ))? {
            println!("Aborted.");
            return Ok(());
        }
    }

    // 6. Write the pruned registry (live entries only).
    save_index_registry_at(toml_path, &live)?;

    println!(
        "{} Removed {} orphaned registration(s) from indexes.toml. {} registration(s) remain.",
        "✓".green(),
        count.to_string().bold(),
        live.len()
    );

    Ok(())
}

/// Why: keep the y/N prompt isolated so tests bypass it via `interactive=false`.
/// What: prints `<prompt> [y/N] ` to stdout, reads one line from stdin, returns
/// `true` when the trimmed reply starts with `y` or `Y`. Empty input → false.
/// Test: side-effect-only; exercised manually.
fn confirm(prompt: &str) -> Result<bool> {
    print!("{} [y/N] ", prompt);
    std::io::stdout().flush().ok();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let answer = line.trim();
    Ok(matches!(answer.chars().next(), Some('y') | Some('Y')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::persistence::{save_index_registry_at, PersistedIndex};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn entry(id: &str, root: &str) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: PathBuf::from(root),
            ..Default::default()
        }
    }

    /// Why: the core contract — an entry whose root_path does not exist must be
    /// removed from indexes.toml by prune-orphans.
    /// What: write a registry with one dead-root entry, run prune_orphans, reload
    /// the registry, assert the dead entry is gone.
    /// Test: this test.
    #[test]
    fn prune_orphans_removes_dead_root_entries() {
        let tmp = tempdir().unwrap();
        let toml_path = tmp.path().join("indexes.toml");

        // Dead root (non-existent path).
        let dead = entry("ghost", "/tmp/trusty-prune-orphans-dead-root-xyz9999");
        // Live root (the tempdir itself exists).
        let live_root = tmp.path().to_path_buf();
        let live = PersistedIndex {
            id: "live".into(),
            root_path: live_root.clone(),
            ..Default::default()
        };

        save_index_registry_at(&toml_path, &[dead, live]).unwrap();
        assert_eq!(
            load_index_registry_at(&toml_path).unwrap().len(),
            2,
            "setup: both entries must be in the registry"
        );

        // Run prune-orphans (non-interactive, no dry-run).
        handle_prune_orphans_at(
            &toml_path, /*dry_run=*/ false, /*yes=*/ true, /*interactive=*/ false,
        )
        .unwrap();

        let remaining = load_index_registry_at(&toml_path).unwrap();
        assert_eq!(remaining.len(), 1, "dead-root entry must be removed");
        assert_eq!(remaining[0].id, "live", "live entry must be preserved");
    }

    /// Why: prune-orphans must NEVER remove entries whose root_path exists.
    /// What: write a registry with only live-root entries, run prune, assert
    /// the registry is unchanged.
    /// Test: this test.
    #[test]
    fn prune_orphans_preserves_live_root_entries() {
        let tmp = tempdir().unwrap();
        let toml_path = tmp.path().join("indexes.toml");

        let live = PersistedIndex {
            id: "myproject".into(),
            root_path: tmp.path().to_path_buf(),
            ..Default::default()
        };
        save_index_registry_at(&toml_path, &[live]).unwrap();

        handle_prune_orphans_at(&toml_path, false, true, false).unwrap();

        let remaining = load_index_registry_at(&toml_path).unwrap();
        assert_eq!(remaining.len(), 1, "live entry must not be removed");
        assert_eq!(remaining[0].id, "myproject");
    }

    /// Why: --dry-run must preview orphans without mutating indexes.toml.
    /// What: write a registry with one dead entry, run with dry_run=true, reload
    /// and assert the entry is still there.
    /// Test: this test.
    #[test]
    fn prune_orphans_dry_run_mutates_nothing() {
        let tmp = tempdir().unwrap();
        let toml_path = tmp.path().join("indexes.toml");

        let dead = entry("ghost", "/tmp/trusty-dry-run-dead-xyz8888");
        save_index_registry_at(&toml_path, &[dead]).unwrap();

        // dry_run=true — must not write to toml_path.
        handle_prune_orphans_at(
            &toml_path, /*dry_run=*/ true, /*yes=*/ true, /*interactive=*/ false,
        )
        .unwrap();

        let after = load_index_registry_at(&toml_path).unwrap();
        assert_eq!(
            after.len(),
            1,
            "dry-run must not modify indexes.toml: found {} entries",
            after.len()
        );
        assert_eq!(
            after[0].id, "ghost",
            "dry-run must leave the orphan in place"
        );
    }

    /// Why: an empty registry must be handled gracefully (no panic, no error).
    /// What: call handle_prune_orphans_at on an empty file, assert it returns Ok.
    /// Test: this test.
    #[test]
    fn prune_orphans_empty_registry_is_noop() {
        let tmp = tempdir().unwrap();
        let toml_path = tmp.path().join("indexes.toml");
        // Don't create the file — load_index_registry_at treats missing as empty.
        let result = handle_prune_orphans_at(&toml_path, false, true, false);
        assert!(result.is_ok(), "empty registry must not error");
    }
}
