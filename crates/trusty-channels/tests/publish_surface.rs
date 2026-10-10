//! Which binaries a default build of trusty-channels produces (#8454 S2c,
//! ruling Q7).
//!
//! Why: a binary that can send with no route check must not be built by
//! default, so `cargo install trusty-channels` never installs one.
//! What: reads this crate's `Cargo.toml` and checks every `[[bin]]`. A binary
//! named in `DEFAULT_BINS` (route-checked, or unable to send) may build by
//! default; every other binary must carry `required-features` naming a
//! feature the default set does not enable. A runtime check cannot do this:
//! Cargo sets `CARGO_BIN_EXE_<name>` for a binary whose required features
//! are off, too.
//! Test: this file.

use std::collections::BTreeSet;

/// Binaries allowed in the default build, with the reason each is safe.
const DEFAULT_BINS: &[(&str, &str)] = &[
    (
        "gchat-mcp",
        "every send passes `GchatChannel::check_egress`",
    ),
    (
        "telegram-mcp",
        "scaffold: every tool call returns NotImplemented",
    ),
];

fn manifest() -> toml::Table {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let text = std::fs::read_to_string(path).expect("read Cargo.toml");
    text.parse().expect("parse Cargo.toml")
}

fn strings(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Every feature the default set turns on, following feature-to-feature edges.
fn default_features(manifest: &toml::Table) -> BTreeSet<String> {
    let features = manifest.get("features").and_then(toml::Value::as_table);
    let mut on = BTreeSet::new();
    let mut todo = vec!["default".to_owned()];
    while let Some(name) = todo.pop() {
        if !on.insert(name.clone()) {
            continue;
        }
        let edges = strings(features.and_then(|f| f.get(&name)));
        todo.extend(
            edges
                .into_iter()
                .filter(|e| !e.contains(':') && !e.contains('/')),
        );
    }
    on
}

/// No binary outside `DEFAULT_BINS` builds by default.
#[test]
fn only_route_checked_or_non_sending_bins_build_by_default() {
    let manifest = manifest();
    let on = default_features(&manifest);
    let bins = manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .expect("[[bin]] entries");
    let mut seen = Vec::new();
    for bin in bins {
        let name = bin
            .get("name")
            .and_then(toml::Value::as_str)
            .expect("bin name");
        seen.push(name.to_owned());
        if DEFAULT_BINS.iter().any(|(allowed, _)| *allowed == name) {
            continue;
        }
        let required = strings(bin.get("required-features"));
        assert!(
            required.iter().any(|f| !on.contains(f)),
            "`{name}` builds by default; a binary that can send without a route \
             check needs `required-features` naming a non-default feature (#8454 Q7)"
        );
    }
    assert!(
        seen.iter().any(|b| b == "slack-mcp"),
        "slack-mcp target missing"
    );
}

/// The opt-in feature exists and is off by default, so operators keep `slack-mcp`.
#[test]
fn unrouted_slack_mcp_feature_is_declared_and_off_by_default() {
    let manifest = manifest();
    let declared = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .is_some_and(|f| f.contains_key("unrouted-slack-mcp"));
    assert!(declared, "feature `unrouted-slack-mcp` is not declared");
    assert!(!default_features(&manifest).contains("unrouted-slack-mcp"));
}
