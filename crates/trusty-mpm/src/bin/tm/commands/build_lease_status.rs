//! The read-only views of `tm build-lease`: `status` and `--census` (#9239).
//!
//! Why: a builder stuck behind a slot seed showed only "waiting", and the
//! lease state was readable only by listing `~/.trusty-mpm/build-slots/` by
//! hand. Neither view takes a lease or runs a build.
//! Test: `build_lease_status_lists_a_seed_in_progress_9239`,
//! `the_census_view_lists_holders_without_argument_values` in
//! `tests/tm_build_lease.rs`.

use std::path::PathBuf;

use trusty_mpm::core::build_lease::census_detail::{header, live_breakdown};
use trusty_mpm::core::build_lease::slots::SlotDir;
use trusty_mpm::core::builder_slot_pool::staging::{StagingKind, pool_status};
use trusty_mpm::core::builders::{BuildersConfig, resolve_max_concurrent};

/// `tm build-lease --census`: print the holders and the attributed census.
///
/// What: runs no build; probes slot locks like `tm doctor`, which creates the
/// store if it is missing (#8261 round 5). Exit 0 on a census read; exit 1
/// naming the error when the process table cannot be read. A store that cannot be opened lists no
/// holders and says so. Argument values never print (see `census_detail`).
/// Test: `the_census_view_lists_holders_without_argument_values`.
pub(crate) fn print_census() -> ! {
    let slots = SlotDir::resolve(dirs::home_dir().as_deref());
    let holders = slots.as_ref().map(SlotDir::holders).unwrap_or_default();
    let details = match live_breakdown(&holders) {
        Ok(details) => details,
        Err(err) => {
            eprintln!("tm build-lease: the process census cannot be read: {err}");
            std::process::exit(1)
        }
    };
    println!(
        "{}",
        header(details.len(), holders.len(), resolve_max_concurrent())
    );
    if let Err(err) = &slots {
        println!("  lease store unusable: {err}");
    }
    for holder in &holders {
        println!("  lease {}", holder.render());
    }
    for (i, detail) in details.iter().enumerate() {
        println!("  group {}: {}", i + 1, detail.render());
    }
    std::process::exit(0)
}

/// `tm build-lease status`: holders, slots, and seeds in progress (#9239).
///
/// Why: issue #9239's closure asks that seeding in progress be visible with
/// its age; a seed that held a release agent for 2h44m was invisible.
/// What: prints the lease holders (as `--census` reads them), then for each
/// repo under `builders.slot_pool_root`: its `slot-<n>` directories as seeded
/// or unseeded, and every staging tree with its slot, owner pid, liveness and
/// age. Lists only — it deletes nothing and takes no lease. Exit 0.
/// Test: `build_lease_status_lists_a_seed_in_progress_9239`.
pub(crate) fn print_status(builders: &BuildersConfig) -> ! {
    let home = dirs::home_dir();
    let home_or_root = home.clone().unwrap_or_else(|| PathBuf::from("/"));
    let slots = SlotDir::resolve(home.as_deref());
    let holders = slots.as_ref().map(SlotDir::holders).unwrap_or_default();
    let root = builders.effective_slot_pool_root(&home_or_root);
    let repos = pool_status(&root);
    let seeding = repos
        .iter()
        .flat_map(|r| &r.staging)
        .filter(|e| e.kind == StagingKind::Seeding)
        .count();
    println!(
        "tm build-lease status: {} lease holder(s), {seeding} seed staging tree(s); pool {}",
        holders.len(),
        root.display()
    );
    if let Err(err) = &slots {
        println!("  lease store unusable: {err}");
    }
    for holder in &holders {
        println!("  lease {}", holder.render());
    }
    for repo in &repos {
        let states = repo
            .slots
            .iter()
            .map(|(n, seeded)| format!("slot-{n} {}", if *seeded { "seeded" } else { "unseeded" }))
            .collect::<Vec<_>>();
        let states = if states.is_empty() {
            "no slots".to_string()
        } else {
            states.join(", ")
        };
        println!("  pool {}: {states}", repo.dir.display());
        for entry in &repo.staging {
            println!("    {}", entry.render());
        }
    }
    std::process::exit(0)
}
