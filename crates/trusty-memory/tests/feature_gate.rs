//! The `server` / `daemon` / `mcp-schema` feature contract (#9269, ADR-0066 D1.4).
//!
//! Why: the trusty-memory 1.x contract freezes the engine library built with
//! `--no-default-features`. That build must not drag in the serving surface's
//! dependencies, and the frozen feature names must keep their meaning. Both
//! are manifest facts that no unit test of the code can see, so this file
//! reads the manifest and the resolved dependency tree directly.
//! What: asks `cargo tree` for the normal-dependency tree of each feature set
//! and checks which of trusty-mcp, clap and rusqlite it contains, then checks
//! the feature table and the `[[bin]]` `required-features` in `Cargo.toml`.
//! Test: this file.

use std::process::Command;

const MANIFEST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

/// The crate names, one per line, of trusty-memory's normal-dependency tree
/// under `features` (the arguments after `cargo tree -p trusty-memory`).
fn dependency_names(features: &[&str]) -> Vec<String> {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "trusty-memory",
            "-e",
            "normal",
            "--prefix",
            "none",
        ])
        .args(features)
        .arg("--manifest-path")
        .arg(MANIFEST)
        .output()
        .expect("`cargo tree` must run");
    assert!(
        output.status.success(),
        "`cargo tree {features:?}` failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// Why: AC1 of #9269 — the `--no-default-features` tree has no trusty-mcp,
/// clap or rusqlite — and the slim `mcp-schema` build adds only trusty-mcp.
/// The default row is the control: it shows the check finds all three when
/// they are present, so an empty tree cannot pass the other rows by accident.
/// What: one `cargo tree` per feature set, compared against the expected
/// presence of each of the three crates.
/// Test: itself.
#[test]
fn each_feature_set_resolves_only_its_own_serving_dependencies() {
    let cases: [(&[&str], [bool; 3]); 3] = [
        // (cargo tree feature args, [trusty-mcp, clap, rusqlite] present?)
        (&[], [true, true, true]),
        (&["--no-default-features"], [false, false, false]),
        (
            &["--no-default-features", "--features", "mcp-schema"],
            [true, false, false],
        ),
    ];
    for (features, expected) in cases {
        let names = dependency_names(features);
        for (krate, want) in ["trusty-mcp", "clap", "rusqlite"].iter().zip(expected) {
            assert_eq!(
                names.iter().any(|n| n == krate),
                want,
                "`cargo tree -p trusty-memory {features:?}`: expected {krate} present={want}"
            );
        }
    }
}

/// Why: AC2 of #9269 and ADR-0066 D1.4 — both binaries ship with the server
/// build only, `server` is the default, and `daemon` stays an alias of it
/// for all of 1.x. A rename or a dropped alias is a 1.x break.
/// What: parses `Cargo.toml` and checks the feature table and each `[[bin]]`
/// `required-features`.
/// Test: itself.
#[test]
fn server_is_the_default_and_gates_both_binaries() {
    let text = std::fs::read_to_string(MANIFEST).expect("read Cargo.toml");
    let manifest: toml::Table = text.parse().expect("Cargo.toml parses");
    let features = manifest["features"].as_table().expect("[features]");
    let list = |name: &str| -> Vec<&str> {
        features[name]
            .as_array()
            .unwrap_or_else(|| panic!("feature `{name}` is a list"))
            .iter()
            .filter_map(toml::Value::as_str)
            .collect()
    };
    assert_eq!(list("default"), ["server"]);
    assert_eq!(list("daemon"), ["server"]);
    assert!(list("server").contains(&"mcp-schema"));
    assert_eq!(list("mcp-schema"), ["dep:trusty-mcp"]);

    let bins = manifest["bin"].as_array().expect("[[bin]]");
    for name in ["trusty-memory", "trusty-memory-mcp-bridge"] {
        let bin = bins
            .iter()
            .find(|b| b["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("[[bin]] {name}"));
        let required: Vec<&str> = bin
            .get("required-features")
            .and_then(toml::Value::as_array)
            .map(|a| a.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();
        assert_eq!(required, ["server"], "[[bin]] {name} required-features");
    }
}
