//! `tm catalog` sync/inspect command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`CatalogAction`] — `sync`/`ls`/`status`/`apply`.
//! Test: `cli_parses_catalog_*` in `tests.rs`.

use clap::Subcommand;

/// Actions for the `catalog` subcommand.
///
/// Why: catalog management splits into a remote-sync operation, a local listing,
/// a staleness check, and the rebuild/redeploy `apply`; separate sub-actions keep
/// each scriptable.
/// What: `Sync` fetches the catalog (respecting a TTL unless `--force`); `Ls`
/// lists cached agents and skills; `Status` reports whether deployed content is
/// stale; `Apply` syncs then redeploys the manifest-selected content (the HR-3
/// rebuild offer made concrete).
/// Test: `cli_parses_catalog_sync`, `cli_parses_catalog_ls`,
/// `cli_parses_catalog_status`, `cli_parses_catalog_apply`.
#[derive(Debug, Subcommand)]
pub(crate) enum CatalogAction {
    /// Fetch or refresh the agent/skill catalog from the claude-mpm repo.
    Sync {
        /// Force a fetch even if the cache TTL has not expired.
        #[arg(long)]
        force: bool,
    },
    /// List the cached agents and skills.
    Ls {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Report whether the deployed content has drifted from the synced catalog.
    ///
    /// Why: the HR-3 staleness check surfaced as a scriptable CLI verb — the same
    /// signal `GET /health` and the TUI use, without mutating anything.
    /// What: compares the deployed checksum manifests against the synced catalog
    /// and prints stale/unknown plus a per-artifact change list.
    /// Test: `cli_parses_catalog_status`.
    Status {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Rebuild/redeploy the manifest-selected content from the catalog (HR-3).
    ///
    /// Why: the HR-3 rebuild OFFER made actionable — accepting it syncs the
    /// catalog then redeploys agents/skills from it, clearing staleness. Never
    /// runs automatically; the operator (or the TUI hint) invokes it explicitly.
    /// What: syncs (honouring the TTL unless `--force`), redeploys the
    /// manifest-selected agents and skills (updating the checksum manifests), and
    /// with `--prune` removes managed agents/skills the manifest no longer selects.
    /// Test: `cli_parses_catalog_apply`; behaviour by `tests/catalog_apply.rs`.
    Apply {
        /// Force a catalog fetch even if the cache TTL has not expired.
        #[arg(long)]
        force: bool,
        /// Also remove managed agents/skills the manifest no longer selects.
        #[arg(long)]
        prune: bool,
    },
}
