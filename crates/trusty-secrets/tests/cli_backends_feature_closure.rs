//! The `cli-backends` feature never reaches trusty-common.
//!
//! Why: #7519, owner ruling 2026-10-07 — "Secrets should have no common
//! dependencies." The CLI runner duplicates trusty-common's `external_cli`
//! rather than depending on it, and a later edit that adds the edge back
//! must fail a test, not wait for a reviewer.
//! What: reads this crate's `Cargo.toml` as text (no TOML dependency),
//! walks each feature's closure through `dep:`, `pkg/feat` and feature
//! references, and checks which optional dependencies it reaches. Needs no
//! feature itself, so it runs in every feature combination.
//! Test: itself.

use std::collections::{BTreeMap, BTreeSet};

/// Each `[features]` entry's raw list, by feature name.
fn features(manifest: &str) -> BTreeMap<String, Vec<String>> {
    let mut table = BTreeMap::new();
    let mut in_features = false;
    let mut pending: Option<(String, String)> = None;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') && pending.is_none() {
            in_features = line == "[features]";
            continue;
        }
        if !in_features || line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (name, mut body) = match pending.take() {
            Some((name, body)) => (name, body + line),
            None => match line.split_once('=') {
                Some((name, body)) => (name.trim().to_string(), body.trim().to_string()),
                None => continue,
            },
        };
        if !body.contains(']') {
            pending = Some((name, body));
            continue;
        }
        body.retain(|c| !matches!(c, '[' | ']' | '"' | ' '));
        let entries = body
            .split(',')
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect();
        table.insert(name, entries);
    }
    table
}

/// The optional dependencies `feature` turns on, transitively.
fn reached_deps(table: &BTreeMap<String, Vec<String>>, feature: &str) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![feature.to_string()];
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(entries) = table.get(&name) else {
            // An implicit feature: the optional dependency of that name.
            deps.insert(name);
            continue;
        };
        for entry in entries {
            if let Some(dep) = entry.strip_prefix("dep:") {
                deps.insert(dep.to_string());
            } else if let Some((pkg, _)) = entry.split_once('/') {
                deps.insert(pkg.trim_end_matches('?').to_string());
            } else {
                stack.push(entry.clone());
            }
        }
    }
    deps
}

fn manifest() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    std::fs::read_to_string(path).expect("read this crate's Cargo.toml")
}

/// Why: see the module docs. The `server` closure is the positive control:
/// it does reach trusty-common, so a parser that saw nothing would fail.
/// Test: itself.
#[test]
fn cli_backends_feature_closure_never_reaches_trusty_common() {
    let manifest = manifest();
    let table = features(&manifest);
    let cli = table
        .get("cli-backends")
        .expect("the `cli-backends` feature exists");
    assert_eq!(cli, &["store"]);

    let reached = reached_deps(&table, "cli-backends");
    assert!(!reached.contains("trusty-common"), "{reached:?}");
    // std plus what `store` already links (libc among it), nothing else.
    assert_eq!(reached, reached_deps(&table, "store"));
    assert!(reached.contains("libc"), "{reached:?}");

    assert!(
        reached_deps(&table, "server").contains("trusty-common"),
        "positive control: `server` keeps its trusty-common edge"
    );
    // A non-optional trusty-common would be reached by every feature.
    for line in manifest.lines().filter(|l| l.starts_with("trusty-common")) {
        assert!(line.contains("optional = true"), "{line}");
    }
}
