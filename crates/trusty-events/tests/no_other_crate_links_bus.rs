//! ADR-0065 decision 1 and acceptance condition 1: no other crate links the
//! bus library. Every client meets the bus on its socket, through the
//! contract in `trusty-common::control_bus` (#9075).

use std::path::Path;
use std::process::Command;

const BUS: &str = "trusty-events";

/// No workspace package names `trusty-events` as a dependency of any kind
/// (normal, dev or build, renamed or not), and the root manifest offers no
/// `[workspace.dependencies]` entry that would make linking it one line.
#[test]
fn no_workspace_crate_links_trusty_events() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(&root)
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata is JSON");
    let packages = metadata["packages"].as_array().expect("packages array");
    assert!(
        packages.len() > 10,
        "scan floor: cargo metadata listed only {} packages",
        packages.len()
    );

    let mut linkers = Vec::new();
    for package in packages {
        let name = package["name"].as_str().unwrap_or_default();
        if name == BUS {
            continue;
        }
        let deps = package["dependencies"]
            .as_array()
            .expect("dependencies array");
        for dep in deps {
            if dep["name"].as_str() == Some(BUS) {
                let kind = dep["kind"].as_str().unwrap_or("normal");
                linkers.push(format!("{name} ({kind})"));
            }
        }
    }
    assert!(
        linkers.is_empty(),
        "ADR-0065 decision 1: these crates link {BUS}: {linkers:?}"
    );

    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("read root manifest");
    let offered = manifest
        .lines()
        .map(str::trim_start)
        .any(|line| line.starts_with(BUS) && line[BUS.len()..].trim_start().starts_with('='));
    assert!(
        !offered,
        "the root manifest declares a `{BUS}` workspace dependency; ADR-0065 decision 1"
    );
}
