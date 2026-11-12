//! `tm build-lease where <bin>`: print the freshest built binary (#10835).
//!
//! Why: a caller or sandbox launcher asked which `debug/tm` is current had to
//! guess between the pool slots and `CARGO_TARGET_DIR`, and ran a stale one
//! (#10755).
//! What: parses the words after `where`, searches the candidate directories
//! from `core::build_lease::locate`, and prints the newest artifact's absolute
//! path (or, with `--json`, its path, source, target dir and mtime). Takes no
//! lease, claims no slot, runs no cargo.
//! Test: `build_lease_where_prints_the_newest_slot_binary_10835`,
//! `build_lease_where_names_every_dir_when_the_binary_is_absent_10835` in
//! `tests/tm_build_lease.rs`; the search itself in `locate_tests.rs`.

use std::path::PathBuf;

use clap::Parser;
use trusty_mpm::core::build_lease::locate::{candidate_dirs, freshest, is_bare_name};
use trusty_mpm::core::build_lease::stale_guard::checkout_root;
use trusty_mpm::core::build_lease::target_dir::resolve_pool;
use trusty_mpm::core::builders::BuildersConfig;

/// The cargo profile whose output directory is searched.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Profile {
    /// `<target>/debug/` (cargo's `dev` profile).
    Debug,
    /// `<target>/release/`.
    Release,
}

impl Profile {
    fn dir(self) -> &'static str {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
    }
}

/// `tm build-lease where` arguments.
#[derive(Debug, Parser)]
#[command(
    name = "tm build-lease where",
    about = "Print the freshest built binary for this checkout (#10835)",
    long_about = "Print the absolute path of the newest existing <target>/<profile>/<BIN> \
                  across the target directory in effect (CARGO_TARGET_DIR, else \
                  <checkout>/target), the repo's shared target directory, and this repo's \
                  seeded build-pool slots. Newest is by the artifact's mtime. Exits 1, \
                  naming every path searched, when none exists. Read-only: takes no lease \
                  and runs no cargo."
)]
struct WhereArgs {
    /// The binary's name, e.g. `tm`.
    bin: String,
    /// Which profile's output directory to search.
    #[arg(long, value_enum, default_value = "debug")]
    profile: Profile,
    /// Print `{path, source, target_dir, mtime_unix}` as JSON.
    #[arg(long)]
    json: bool,
}

/// Run `tm build-lease where <rest…>` and exit.
///
/// Why: scripts need the path cargo actually wrote last, not a guess.
/// What: exit 0 with the path (or JSON) on stdout; exit 1 with stdout empty
/// and every searched path on stderr when no artifact exists; clap's usage
/// exit (2) on bad arguments or a binary name that is a path.
/// Test: `build_lease_where_prints_the_newest_slot_binary_10835`,
/// `build_lease_where_names_every_dir_when_the_binary_is_absent_10835`.
pub(crate) fn print_where(rest: &[String], builders: &BuildersConfig) -> ! {
    let args = WhereArgs::try_parse_from(
        std::iter::once("tm build-lease where".to_string()).chain(rest.iter().cloned()),
    )
    .unwrap_or_else(|e| e.exit());
    if !is_bare_name(&args.bin) {
        eprintln!(
            "tm build-lease where: `{}` is not a binary name; pass the bare name, e.g. `tm`",
            args.bin
        );
        std::process::exit(2)
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let checkout = checkout_root(&cwd);
    let env_target = std::env::var_os("CARGO_TARGET_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    // A checkout with no origin has no pool; the target dirs are still searched.
    let (pool_dir, shared) = match resolve_pool(builders, &home, &checkout) {
        Ok((pool, shared)) => (Some(pool.repo_dir()), shared),
        Err(why) => {
            eprintln!("tm build-lease where: no build pool searched — {why}");
            (None, None)
        }
    };
    let candidates = candidate_dirs(
        &checkout,
        env_target.as_deref(),
        shared.as_deref(),
        pool_dir.as_deref(),
    );
    match freshest(&candidates, &args.bin, args.profile.dir()) {
        Ok(found) if args.json => match serde_json::to_string(&found) {
            Ok(json) => println!("{json}"),
            Err(err) => {
                eprintln!("tm build-lease where: could not encode the result: {err}");
                std::process::exit(1)
            }
        },
        Ok(found) => println!("{}", found.path.display()),
        Err(searched) => {
            eprintln!(
                "tm build-lease where: no {} `{}` built for {}; searched:",
                args.profile.dir(),
                args.bin,
                checkout.display()
            );
            for path in &searched {
                eprintln!("  {}", path.display());
            }
            std::process::exit(1)
        }
    }
    std::process::exit(0)
}
