//! Build script: mint ONE build id per package build, shared by every target
//! this package compiles (#7873).
//!
//! Why: #7822 fingerprinted the running executable as `"<mtime>:<size>"` so
//! `tm doctor`'s `daemon_version` check could tell two same-version builds
//! apart. That fingerprint is per-FILE, and this package ships two `[[bin]]`
//! targets from one source: `tm` runs the check, `trusty-mpm` runs the daemon.
//! One `cargo install` writes them seconds apart, so their fingerprints differ
//! by construction — the observed pair was daemon `1789397245:73209856` against
//! installed `1789397221:73195712` on a fully current install — and the Warn
//! could never clear. A build id has to be a property of the BUILD, not of the
//! file a given process happens to have been launched from, and a build script
//! is the only place in a Cargo package that runs exactly once per build.
//!
//! What: emits `TRUSTY_MPM_BUILD_ID` as nanoseconds since the Unix epoch at the
//! moment this script runs, via `cargo::rustc-env`. Cargo runs the script once
//! and hands the same value to every target it then compiles, so `tm` and
//! `trusty-mpm` carry identical ids; `core::build_identity::build_id` reads it
//! back with `env!`. The `cargo::rerun-if-changed` list below is what makes the
//! id change on a real rebuild and hold still otherwise: Cargo replays a cached
//! script's output when none of the named paths moved, so an unchanged tree
//! keeps its id instead of minting a new one on every `cargo build` — which is
//! also why this cannot loop, the script writes nothing back into the tree.
//!
//! The list names this package's own compiled-in inputs (`src`, the `assets`
//! and `docs` trees reached by `include_str!`, `help.yaml`, the manifest, this
//! file) plus the workspace lockfile, which catches a dependency version
//! change. A source edit in a sibling PATH crate is the one rebuild this does
//! not observe: it relinks both bins without re-running this script, so the id
//! carries over. Watching `crates/` wholesale would fix that at the cost of
//! stat-ing every crate's tree — including the Svelte UI `node_modules` — on
//! every build, which is the worse trade for a diagnostic field.
//!
//! No git SHA. A correct one needs the per-worktree `HEAD` / `logs/HEAD` / ref
//! watching that `crates/trusty-audit/build.rs` spells out over 130 lines,
//! because a SHA captured by a cached script silently goes stale; the id's job
//! is to differ between builds, not to name a commit.
//!
//! Test: `core::build_identity::tests::build_id_does_not_vary_with_the_executable_file`
//! fails if this script stops being the id's source;
//! `build_id_is_a_nonempty_decimal_counter` pins the shape. A build script has
//! no `cargo test` target of its own.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Declared only when the path EXISTS: Cargo treats a missing
    // `rerun-if-changed` path as perpetually changed, so naming one would
    // re-run this script — and mint a new id — on every single build. The
    // workspace lockfile is the path that can be absent, in a crates.io source
    // tarball, which has no workspace above it.
    for path in [
        "src",
        "assets",
        "docs",
        "help.yaml",
        "Cargo.toml",
        "build.rs",
        "../../Cargo.lock",
    ] {
        if std::path::Path::new(path).exists() {
            println!("cargo::rerun-if-changed={path}");
        }
    }

    // A clock before the Unix epoch is not a reason to fail a build; `0` is a
    // valid id that simply never changes, and the check degrades to the
    // version comparison it had before #7822.
    let build_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_nanos());
    println!("cargo::rustc-env=TRUSTY_MPM_BUILD_ID={build_id}");
}
