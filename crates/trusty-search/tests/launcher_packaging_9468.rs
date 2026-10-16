//! Packaging of the `trusty-embedderd-py` launcher (#9468).
//!
//! Why: the daemon finds the launcher beside its own executable, so only a
//! binary that `cargo install trusty-search` installs is ever found. While the
//! trusty-embedderd-py package owned it, no install path produced it.
//! What: reads `cargo metadata --no-deps` for the workspace and checks that
//! trusty-search declares the launcher bin, that no other package declares a
//! binary trusty-search ships (`cargo install` refuses a name another package
//! owns), and that trusty-common's `CRATE_BINARIES` row matches the manifest.
//! Test: this file.

use std::collections::BTreeSet;
use std::process::Command;

const LAUNCHER: &str = "trusty-embedderd-py";

/// `(package name, bin target names)` for every workspace member.
fn workspace_bins() -> Vec<(String, BTreeSet<String>)> {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let out = Command::new(option_env!("CARGO").unwrap_or("cargo"))
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .args(["--manifest-path", manifest])
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata JSON");
    let is_bin = |t: &&serde_json::Value| {
        t["kind"]
            .as_array()
            .is_some_and(|k| k.iter().any(|k| k == "bin"))
    };
    let packages = meta["packages"].as_array().expect("packages array");
    packages
        .iter()
        .map(|p| {
            let targets = p["targets"].as_array().into_iter().flatten();
            let bins = targets
                .filter(is_bin)
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect();
            (p["name"].as_str().unwrap_or_default().to_owned(), bins)
        })
        .collect()
}

/// trusty-search alone declares the launcher bin, and tctl's table agrees.
#[test]
fn launcher_bin_ships_with_trusty_search_only() {
    let packages = workspace_bins();
    let (_, search) = packages
        .iter()
        .find(|(name, _)| name == "trusty-search")
        .expect("trusty-search package");
    assert!(
        search.contains(LAUNCHER),
        "trusty-search must declare the `{LAUNCHER}` bin so `cargo install \
         trusty-search` installs it (#9468); it declares {search:?}"
    );
    for (name, bins) in packages.iter().filter(|(n, _)| n != "trusty-search") {
        let clash: Vec<&String> = bins.intersection(search).collect();
        assert!(
            clash.is_empty(),
            "package `{name}` also declares {clash:?}; two packages must never \
             install a binary of the same name (#9468, #1318)"
        );
    }
    let table: BTreeSet<String> = trusty_common::bin_resolve::installed_binaries("trusty-search")
        .into_iter()
        .collect();
    assert_eq!(
        &table, search,
        "trusty-common CRATE_BINARIES must list exactly trusty-search's bins"
    );
}
