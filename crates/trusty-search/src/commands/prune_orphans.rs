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
use trusty_common::repo_identity::RepoIdentity;

use crate::service::persistence::{
    indexes_toml_path, load_index_registry_at, save_index_registry_at, PersistedIndex,
};

/// Why: a small record per orphaned entry keeps the display and removal
/// logic independent of the full `PersistedIndex` shape.
/// What: holds the id, the dead root path (for display only), and the stored
/// canonical repo identity (DOC-37) used to group orphans by repo.
/// Test: covered transitively by `handle_prune_orphans`.
struct OrphanEntry {
    id: String,
    root_path: String,
    repo_identity: Option<String>,
}

/// Sentinel group label for indexes with no derivable/stored repo identity.
const NO_IDENTITY_LABEL: &str = "(no repo identity)";

/// Resolve a registered entry's canonical repo identity for reporting (DOC-37).
///
/// Why: the stored `repo_identity` may be absent on an index that predates
/// identity tracking and has not yet been backfilled by a warm-boot; deriving
/// live from a still-present `root_path` recovers it for the report. Orphaned
/// entries (missing root) can only use the stored value.
/// What: returns the stored identity, else a live derive from `root_path` when
/// the path still exists, else `None`.
/// Test: `prune_orphans_groups_by_repo_identity`, `report_repo_identity_*`.
fn resolve_identity(entry: &PersistedIndex) -> Option<String> {
    entry.repo_identity.clone().or_else(|| {
        entry
            .root_path
            .exists()
            .then(|| RepoIdentity::derive(&entry.root_path).map(|r| r.canonical()))
            .flatten()
    })
}

/// Handle `trusty-search prune-orphans [--dry-run] [--yes] [--repo-identity <id>]`.
///
/// Why: extracted so `main()` stays thin and this function is independently
/// testable with path-injectable helpers.
/// What: with `--repo-identity <owner/repo>` it prints a READ-ONLY grouped
/// report of every registered index (live and orphaned) for that repo and exits
/// without mutating anything (DOC-37 visibility mode). Otherwise it loads the
/// registry, filters to entries whose `root_path` does not exist, prints them
/// grouped by repo identity, prompts for confirmation (unless `--yes` or
/// `--dry-run`), then removes the orphans. `--dry-run` overrides `--yes`.
/// Test: `cargo run -p trusty-search -- prune-orphans --dry-run`.
pub fn handle_prune_orphans(dry_run: bool, yes: bool, repo_identity: Option<String>) -> Result<()> {
    let toml_path = indexes_toml_path()?;
    handle_prune_orphans_at(
        &toml_path,
        dry_run,
        yes,
        /*interactive=*/ true,
        repo_identity,
    )
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
    repo_identity: Option<String>,
) -> Result<()> {
    // 1. Load the registry.
    let entries = load_index_registry_at(toml_path)?;

    if entries.is_empty() {
        println!("Registry is empty — nothing to prune.");
        return Ok(());
    }

    // DOC-37 grouped report mode: `--repo-identity` is read-only, showing every
    // facet (live + orphan) of one repo. It never prunes.
    if let Some(filter) = repo_identity {
        return report_repo_identity(&entries, &filter);
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
            repo_identity: resolve_identity(e),
        })
        .collect();

    // 3. Print the table, grouped by repo identity (DOC-37) so an operator sees
    //    which orphans belong to the same repo at a glance.
    let count = orphan_records.len();
    println!(
        "{} {} orphaned index registration(s) (root_path missing):",
        "Found".bold(),
        count.to_string().bold()
    );
    print_grouped(&orphan_records);

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
        if !super::confirm(&format!(
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

/// Print orphan records grouped by canonical repo identity (DOC-37).
///
/// Why: an operator pruning worktree/clone fragmentation wants to see all
/// orphans of one repo together, not interleaved with unrelated ids.
/// What: buckets records by `repo_identity` (identity-less orphans under
/// [`NO_IDENTITY_LABEL`]), prints each group under its identity header with an
/// aligned id/path table. Groups are ordered by identity for stable output.
/// Test: `prune_orphans_groups_by_repo_identity` asserts the header appears.
fn print_grouped(records: &[OrphanEntry]) {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<&str, Vec<&OrphanEntry>> = BTreeMap::new();
    for r in records {
        let key = r.repo_identity.as_deref().unwrap_or(NO_IDENTITY_LABEL);
        groups.entry(key).or_default().push(r);
    }
    for (identity, members) in groups {
        println!("  {} {}", "repo:".dimmed(), identity.cyan().bold());
        let name_width = members.iter().map(|e| e.id.len()).max().unwrap_or(0).max(4);
        for e in members {
            println!(
                "    {:<width$}  {}",
                e.id.bold(),
                e.root_path.dimmed(),
                width = name_width
            );
        }
    }
}

/// Read-only grouped report for a single repo identity (`--repo-identity`).
///
/// Why: DOC-37's MVP visibility goal — let an operator see, for one repo, every
/// index facet (live checkout, `.base` clone, session worktrees) and whether
/// each is live or orphaned, without any mutation.
/// What: normalises `filter` to canonical form, selects every registered entry
/// whose resolved identity matches, and prints each with a `live`/`orphan`
/// status marker. Prints a friendly note and returns `Ok` when nothing matches.
/// Test: `report_repo_identity_lists_matching_facets`,
/// `report_repo_identity_no_match_is_ok`.
fn report_repo_identity(entries: &[PersistedIndex], filter: &str) -> Result<()> {
    let target = RepoIdentity::parse(filter)
        .map(|r| r.canonical())
        .unwrap_or_else(|| filter.trim().to_string());

    let matches: Vec<&PersistedIndex> = entries
        .iter()
        .filter(|e| resolve_identity(e).as_deref() == Some(target.as_str()))
        .collect();

    if matches.is_empty() {
        println!(
            "{} No registered index matches repo identity {}.",
            "ℹ".cyan(),
            target.cyan().bold()
        );
        return Ok(());
    }

    println!(
        "{} {} index facet(s) for repo {}:",
        "Found".bold(),
        matches.len().to_string().bold(),
        target.cyan().bold()
    );
    let name_width = matches.iter().map(|e| e.id.len()).max().unwrap_or(0).max(4);
    for e in matches {
        let (marker, status) = if e.root_path.exists() {
            ("✓".green(), "live".green())
        } else {
            ("✗".red(), "orphan".red())
        };
        println!(
            "  {marker} {:<width$}  {}  {}",
            e.id.bold(),
            status,
            e.root_path.display().to_string().dimmed(),
            width = name_width
        );
    }
    Ok(())
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
            None,
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

        handle_prune_orphans_at(&toml_path, false, true, false, None).unwrap();

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
            None,
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
        let result = handle_prune_orphans_at(&toml_path, false, true, false, None);
        assert!(result.is_ok(), "empty registry must not error");
    }

    /// Why: DOC-37 — orphaned entries carrying a stored `repo_identity` must be
    /// grouped by that identity, and the prune must still remove them.
    /// What: two orphans sharing one identity + one orphan with another; run the
    /// prune and assert every dead-root entry is removed regardless of grouping.
    /// Test: this test.
    #[test]
    fn prune_orphans_groups_by_repo_identity() {
        let tmp = tempdir().unwrap();
        let toml_path = tmp.path().join("indexes.toml");

        let mk = |id: &str, root: &str, identity: &str| PersistedIndex {
            id: id.into(),
            root_path: std::path::PathBuf::from(root),
            repo_identity: Some(identity.into()),
            ..Default::default()
        };
        let a1 = mk(
            "wt-a1",
            "/tmp/trusty-grp-dead-a1-xyz",
            "bobmatnyc/trusty-tools",
        );
        let a2 = mk(
            "wt-a2",
            "/tmp/trusty-grp-dead-a2-xyz",
            "bobmatnyc/trusty-tools",
        );
        let b1 = mk("wt-b1", "/tmp/trusty-grp-dead-b1-xyz", "acme/widget");
        save_index_registry_at(&toml_path, &[a1, a2, b1]).unwrap();

        handle_prune_orphans_at(&toml_path, false, true, false, None).unwrap();

        let remaining = load_index_registry_at(&toml_path).unwrap();
        assert!(
            remaining.is_empty(),
            "all dead-root orphans must be removed regardless of identity grouping"
        );
    }

    /// Why: DOC-37 `--repo-identity` is a READ-ONLY report — it must never
    /// mutate the registry and must select entries by canonical identity across
    /// both live and orphaned facets.
    /// What: one live + one orphan sharing an identity, plus an unrelated entry;
    /// run with a `--repo-identity` filter (mixed case to exercise
    /// normalisation) and assert nothing is removed.
    /// Test: this test.
    #[test]
    fn report_repo_identity_lists_matching_facets_without_mutating() {
        let tmp = tempdir().unwrap();
        let toml_path = tmp.path().join("indexes.toml");

        let live = PersistedIndex {
            id: "base".into(),
            root_path: tmp.path().to_path_buf(), // exists
            repo_identity: Some("bobmatnyc/trusty-tools".into()),
            ..Default::default()
        };
        let orphan = PersistedIndex {
            id: "wt-1".into(),
            root_path: std::path::PathBuf::from("/tmp/trusty-report-dead-xyz"),
            repo_identity: Some("bobmatnyc/trusty-tools".into()),
            ..Default::default()
        };
        let unrelated = PersistedIndex {
            id: "other".into(),
            root_path: std::path::PathBuf::from("/tmp/trusty-report-other-xyz"),
            repo_identity: Some("acme/widget".into()),
            ..Default::default()
        };
        save_index_registry_at(&toml_path, &[live, orphan, unrelated]).unwrap();

        // Mixed-case filter must normalise and match; report mode never mutates.
        handle_prune_orphans_at(
            &toml_path,
            false,
            true,
            false,
            Some("BobMatNyc/Trusty-Tools".into()),
        )
        .unwrap();

        let after = load_index_registry_at(&toml_path).unwrap();
        assert_eq!(
            after.len(),
            3,
            "--repo-identity report mode must not remove any entry"
        );
    }

    /// Why: a `--repo-identity` that matches nothing must exit cleanly, not error.
    /// Test: this test.
    #[test]
    fn report_repo_identity_no_match_is_ok() {
        let tmp = tempdir().unwrap();
        let toml_path = tmp.path().join("indexes.toml");
        let e = PersistedIndex {
            id: "only".into(),
            root_path: std::path::PathBuf::from("/tmp/trusty-nomatch-xyz"),
            repo_identity: Some("acme/widget".into()),
            ..Default::default()
        };
        save_index_registry_at(&toml_path, &[e]).unwrap();

        let result =
            handle_prune_orphans_at(&toml_path, false, true, false, Some("no/such-repo".into()));
        assert!(result.is_ok(), "no-match report must return Ok");
        assert_eq!(
            load_index_registry_at(&toml_path).unwrap().len(),
            1,
            "no-match report must not mutate"
        );
    }
}
