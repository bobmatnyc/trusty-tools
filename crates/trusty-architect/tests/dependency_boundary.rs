//! #9171 (a): `trusty-architect` is a leaf. `tm fleet init` calls it, so a
//! dependency on `trusty-mpm`, or on any workspace crate that reaches
//! `trusty-mpm`, would be a cycle.
//!
//! What: reads this crate's `Cargo.toml` and every workspace member's, builds
//! the set {`trusty-mpm`} plus every member whose normal or build dependencies
//! reach it, and fails if any dependency table of this crate (normal, dev,
//! build, and their `[target.*]` forms) names a crate in that set. Renames
//! (`package = "…"`, also through `[workspace.dependencies]`) resolve to the
//! real package name.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

const FORBIDDEN_ROOT: &str = "trusty-mpm";

fn parse(text: &str) -> Table {
    text.parse::<Table>().expect("manifest parses as TOML")
}

fn read_manifest(path: &Path) -> Table {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    parse(&text)
}

/// The dependency tables of `manifest`: top-level and every `[target.*]` form.
/// Dev-dependencies are included only when `with_dev`, because a dependency's
/// dev-dependencies never enter its dependents' graph.
fn dep_tables(manifest: &Table, with_dev: bool) -> Vec<&Table> {
    let kinds: &[&str] = if with_dev {
        &["dependencies", "dev-dependencies", "build-dependencies"]
    } else {
        &["dependencies", "build-dependencies"]
    };
    let targets = manifest
        .get("target")
        .and_then(Value::as_table)
        .into_iter()
        .flat_map(|t| t.values().filter_map(Value::as_table));
    std::iter::once(manifest)
        .chain(targets)
        .flat_map(|scope| kinds.iter().filter_map(|k| scope.get(*k)?.as_table()))
        .collect()
}

/// The real package names `manifest` depends on, resolving `package = "…"`
/// renames locally and through `workspace_deps` for `workspace = true` entries.
fn dep_packages(manifest: &Table, with_dev: bool, workspace_deps: &Table) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for table in dep_tables(manifest, with_dev) {
        for (key, spec) in table {
            let inherited = spec.get("workspace").and_then(Value::as_bool) == Some(true);
            let renamed = spec.get("package").and_then(Value::as_str).or_else(|| {
                inherited
                    .then(|| workspace_deps.get(key)?.get("package")?.as_str())
                    .flatten()
            });
            names.insert(renamed.unwrap_or(key).to_owned());
        }
    }
    names
}

/// {`trusty-mpm`} plus every member whose dependencies reach it, transitively.
fn forbidden_closure(members: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    let mut forbidden = BTreeSet::from([FORBIDDEN_ROOT.to_owned()]);
    loop {
        let before = forbidden.len();
        for (name, deps) in members {
            if deps.iter().any(|d| forbidden.contains(d)) {
                forbidden.insert(name.clone());
            }
        }
        if forbidden.len() == before {
            return forbidden;
        }
    }
}

/// Every workspace member directory, expanding `dir/*` member globs.
fn member_dirs(root: &Path, root_manifest: &Table) -> Vec<PathBuf> {
    let members = root_manifest["workspace"]["members"]
        .as_array()
        .expect("[workspace].members is an array");
    let mut dirs = Vec::new();
    for member in members.iter().filter_map(Value::as_str) {
        match member.strip_suffix("/*") {
            Some(parent) => {
                let entries = fs::read_dir(root.join(parent)).expect("read member glob dir");
                dirs.extend(entries.filter_map(Result::ok).map(|e| e.path()));
            }
            None => dirs.push(root.join(member)),
        }
    }
    dirs.retain(|d| d.join("Cargo.toml").is_file());
    dirs
}

/// The members' normal and build dependencies, keyed by package name.
fn workspace_graph(root: &Path, root_manifest: &Table) -> BTreeMap<String, BTreeSet<String>> {
    let workspace_deps = workspace_deps(root_manifest);
    member_dirs(root, root_manifest)
        .into_iter()
        .map(|dir| {
            let manifest = read_manifest(&dir.join("Cargo.toml"));
            let name = manifest["package"]["name"]
                .as_str()
                .expect("[package].name is a string")
                .to_owned();
            (name, dep_packages(&manifest, false, &workspace_deps))
        })
        .collect()
}

fn workspace_deps(root_manifest: &Table) -> Table {
    root_manifest
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default()
}

#[test]
fn manifest_has_no_trusty_mpm_dependency() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = crate_dir
        .ancestors()
        .nth(2)
        .expect("crate sits at <root>/crates/<name>");
    let root_manifest = read_manifest(&root.join("Cargo.toml"));
    let graph = workspace_graph(root, &root_manifest);
    assert!(
        graph.contains_key(FORBIDDEN_ROOT) && graph.contains_key("trusty-architect"),
        "member scan found neither trusty-mpm nor this crate; it read the wrong tree"
    );
    let forbidden = forbidden_closure(&graph);

    let own = read_manifest(&crate_dir.join("Cargo.toml"));
    let found: Vec<_> = dep_packages(&own, true, &workspace_deps(&root_manifest))
        .intersection(&forbidden)
        .cloned()
        .collect();
    assert!(
        found.is_empty(),
        "trusty-architect must not depend on trusty-mpm or a crate that reaches it; found {found:?}"
    );
}

#[test]
fn boundary_check_resolves_every_dependency_form() {
    let workspace_deps = parse(r#"aliased = { path = "x", package = "dev-only" }"#);
    let manifest = parse(
        r#"
        [dependencies]
        serde = "1"
        uses-mpm = { workspace = true }
        [build-dependencies]
        tm = { path = "../trusty-mpm", package = "trusty-mpm" }
        [target.'cfg(unix)'.dev-dependencies]
        aliased = { workspace = true }
        "#,
    );
    assert_eq!(
        dep_packages(&manifest, true, &workspace_deps),
        BTreeSet::from(["dev-only", "serde", "trusty-mpm", "uses-mpm"].map(String::from))
    );
    assert_eq!(
        dep_packages(&manifest, false, &workspace_deps),
        BTreeSet::from(["serde", "trusty-mpm", "uses-mpm"].map(String::from)),
        "a dev-dependency never enters a dependent's graph"
    );
}

#[test]
fn forbidden_closure_follows_dependents_transitively() {
    let graph = BTreeMap::from([
        ("a".to_owned(), BTreeSet::from(["trusty-mpm".to_owned()])),
        ("b".to_owned(), BTreeSet::from(["a".to_owned()])),
        ("c".to_owned(), BTreeSet::from(["b".to_owned()])),
        ("d".to_owned(), BTreeSet::from(["serde".to_owned()])),
    ]);
    assert_eq!(
        forbidden_closure(&graph),
        BTreeSet::from(["a", "b", "c", "trusty-mpm"].map(String::from))
    );
}
