//! The `cli-backends` and `ancestry` features never reach trusty-common.
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

/// One `[dependencies]`, `[target.*.dependencies]` or `[build-dependencies]`
/// entry: the key features name it by, its real crate name, and whether it
/// is optional. `[dev-dependencies]` never reach a consumer and are skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dep {
    key: String,
    name: String,
    optional: bool,
}

/// Where a section header puts the lines below it.
enum Section {
    /// Not a dependency table this test reads.
    Other,
    /// `key = ...` lines, one entry each.
    Inline,
    /// `[dependencies.<key>]`: the lines below describe one entry.
    Table(usize),
}

/// The quoted value of `field = "..."` in `text`, if present.
fn quoted_field(text: &str, field: &str) -> Option<String> {
    let rest = text.split(&format!("{field} = \"")).nth(1)?;
    rest.split('"').next().map(str::to_string)
}

/// Every normal and build dependency entry, inline and table forms (#9066).
fn dependencies(manifest: &str) -> Vec<Dep> {
    let mut deps: Vec<Dep> = Vec::new();
    let mut section = Section::Other;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Drop quoted target cfgs, whose dots are not path separators.
            let mut header = String::new();
            let mut quote = None;
            for c in line.trim_matches(|c| c == '[' || c == ']').chars() {
                match (quote, c) {
                    (None, '\'' | '"') => quote = Some(c),
                    (Some(q), c) if c == q => quote = None,
                    (None, c) => header.push(c),
                    _ => {}
                }
            }
            let parts: Vec<&str> = header.split('.').collect();
            let at = parts
                .iter()
                .position(|p| *p == "dependencies" || *p == "build-dependencies")
                .filter(|&i| i == 0 || parts[0] == "target");
            section = match at.map(|i| parts.get(i + 1)) {
                None => Section::Other,
                Some(None) => Section::Inline,
                Some(Some(key)) => {
                    deps.push(Dep {
                        key: key.to_string(),
                        name: key.to_string(),
                        optional: false,
                    });
                    Section::Table(deps.len() - 1)
                }
            };
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        match section {
            Section::Other => {}
            Section::Inline => {
                let Some((key, body)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim().to_string();
                deps.push(Dep {
                    name: quoted_field(body, "package").unwrap_or_else(|| key.clone()),
                    optional: body.contains("optional = true"),
                    key,
                });
            }
            Section::Table(i) => {
                if let Some(name) = quoted_field(line, "package") {
                    deps[i].name = name;
                }
                if line.replace(' ', "") == "optional=true" {
                    deps[i].optional = true;
                }
            }
        }
    }
    deps
}

/// The real crate names `feature` turns on, transitively.
///
/// What: `dep:<key>`, `<key>/<feat>` and an implicit feature each name an
/// entry by its key; the key is resolved through `package =` to the crate.
fn reached_deps(
    table: &BTreeMap<String, Vec<String>>,
    deps: &[Dep],
    feature: &str,
) -> BTreeSet<String> {
    let real = |key: &str| {
        deps.iter()
            .find(|d| d.key == key)
            .map_or_else(|| key.to_string(), |d| d.name.clone())
    };
    let mut reached = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![feature.to_string()];
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(entries) = table.get(&name) else {
            // An implicit feature: the optional dependency of that key.
            reached.insert(real(&name));
            continue;
        };
        for entry in entries {
            if let Some(dep) = entry.strip_prefix("dep:") {
                reached.insert(real(dep));
            } else if let Some((pkg, _)) = entry.split_once('/') {
                reached.insert(real(pkg.trim_end_matches('?')));
            } else {
                stack.push(entry.clone());
            }
        }
    }
    reached
}

/// The closure rules for `ancestry` and `cli-backends`, on any manifest text.
///
/// Why: #9066 PR2a review — reading `[features]` alone missed a non-optional
/// dependency (reached by every feature) and a renamed `package =` alias.
/// What: no non-optional normal or build dependency at all; no entry whose
/// real name is trusty-common unless optional; `ancestry` reaches exactly
/// `thiserror` and `libc`; `cli-backends` reaches what `store` reaches and
/// no trusty-common; `server` still reaches keyring and trusty-common.
fn check_closure(manifest: &str) -> Result<(), String> {
    let table = features(manifest);
    let deps = dependencies(manifest);
    let always: Vec<&str> = deps
        .iter()
        .filter(|d| !d.optional)
        .map(|d| d.name.as_str())
        .collect();
    if !always.is_empty() {
        return Err(format!("non-optional dependencies: {always:?}"));
    }
    if let Some(d) = deps
        .iter()
        .find(|d| d.name == "trusty-common" && !d.optional)
    {
        return Err(format!("non-optional trusty-common: {d:?}"));
    }
    let ancestry = reached_deps(&table, &deps, "ancestry");
    let expected: BTreeSet<String> = ["thiserror", "libc"].map(String::from).into();
    if ancestry != expected {
        return Err(format!("`ancestry` reaches {ancestry:?}"));
    }
    let cli = reached_deps(&table, &deps, "cli-backends");
    if cli.contains("trusty-common") || cli != reached_deps(&table, &deps, "store") {
        return Err(format!("`cli-backends` reaches {cli:?}"));
    }
    let server = reached_deps(&table, &deps, "server");
    if !(server.contains("keyring") && server.contains("trusty-common")) {
        return Err(format!(
            "positive control lost: `server` reaches {server:?}"
        ));
    }
    Ok(())
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
    let reached = reached_deps(&table, &dependencies(&manifest), "cli-backends");
    // std plus what `store` already links (libc among it), nothing else.
    assert!(reached.contains("libc"), "{reached:?}");
    assert_eq!(check_closure(&manifest), Ok(()));
}

/// Why: #9066, Architect ruling Q1 (2026-10-11) — trusty-console links
/// `ancestry` to judge its HTTP client, and must never link a store, a
/// keychain, the server or trusty-common (#7519, "no common dependencies").
/// A later edit that widens the closure fails here. `server` is the positive
/// control: it implies `ancestry` and reaches the store and trusty-common.
/// Test: itself.
#[test]
fn ancestry_feature_reaches_no_store_server_or_trusty_common() {
    let manifest = manifest();
    let table = features(&manifest);
    let entries = table
        .get("ancestry")
        .expect("the `ancestry` feature exists");
    assert_eq!(entries, &["dep:thiserror", "dep:libc"]);
    let server = table.get("server").expect("the `server` feature exists");
    assert!(server.iter().any(|e| e == "ancestry"), "{server:?}");
    assert_eq!(check_closure(&manifest), Ok(()));
}

/// Why: #9066 PR2a review — each rule [`check_closure`] enforces must be
/// able to fail. Each mutation of the real manifest is one way the closure
/// could widen, and each must be refused.
/// Test: itself.
#[test]
fn closure_check_refuses_each_widening_mutation() {
    let real = manifest();
    let section = "[dependencies]\n";
    assert!(real.contains(section));
    let add = |entry: &str| real.replacen(section, &format!("{section}{entry}\n"), 1);
    let mutations = [
        (
            "a non-optional dependency",
            add("foo = { workspace = true }"),
            "non-optional",
        ),
        (
            "an aliased trusty-common reached by `ancestry`",
            add(r#"tc = { package = "trusty-common", workspace = true, optional = true }"#)
                .replace(
                    r#"ancestry = ["dep:thiserror", "dep:libc"]"#,
                    r#"ancestry = ["dep:thiserror", "dep:libc", "dep:tc"]"#,
                ),
            "trusty-common",
        ),
        (
            "a non-optional trusty-common",
            real.replace(
                r#"trusty-common = { workspace = true, optional = true,"#,
                r#"trusty-common = { workspace = true,"#,
            ),
            "trusty-common",
        ),
        (
            "a table-form non-optional dependency",
            format!("{real}\n[target.'cfg(unix)'.dependencies.bar]\nversion = \"1\"\n"),
            "bar",
        ),
    ];
    for (what, mutated, named) in mutations {
        assert_ne!(mutated, real, "{what}: the mutation applied");
        let refused = check_closure(&mutated).expect_err(what);
        assert!(refused.contains(named), "{what}: {refused}");
    }
}

/// Why: #9071, Architect ruling D1 — the tier-3 client lives under `api`
/// and may widen it by serde_json and libc only, both already optional
/// dependencies. trusty-console links `api`, so tokio, keyring, fd-lock or
/// trusty-common here would reach it (Df0).
/// Test: itself.
#[test]
fn api_feature_reaches_serde_thiserror_serde_json_libc_only() {
    let manifest = manifest();
    let reached = reached_deps(&features(&manifest), &dependencies(&manifest), "api");
    let expected: BTreeSet<String> = ["serde", "thiserror", "serde_json", "libc"]
        .map(String::from)
        .into();
    assert_eq!(reached, expected);
}
