//! Tests for the deterministic dependency inventory (#2357).
//!
//! Why: the inventory is `measured` data an acquirer relies on; per-ecosystem
//! parsing and lockfile enrichment must be exact and never error on malformed
//! input.
//! What: covers npm (+package-lock), Cargo (+Cargo.lock), pyproject (PEP 621 +
//! poetry), go.mod, the row cap + overflow count, and multi-ecosystem merge.
//! Test: included as `#[cfg(test)] mod tests` from `deps.rs`.

use std::fs;
use std::path::Path;

use super::*;

fn write(root: &Path, name: &str, content: &str) {
    fs::write(root.join(name), content).unwrap();
}

/// Why: npm declared deps must carry their locked versions from package-lock.
/// What: writes a package.json + v3 lock; asserts the spec and locked version.
/// Test: this test itself.
#[test]
fn npm_manifest_and_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"dependencies": {"react": "^18.0.0"}, "devDependencies": {"jest": "^29"}}"#,
    );
    write(
        tmp.path(),
        "package-lock.json",
        r#"{"lockfileVersion": 3, "packages": {"": {}, "node_modules/react": {"version": "18.2.0"}, "node_modules/jest": {"version": "29.7.0"}}}"#,
    );
    let inv = build_inventory(tmp.path());
    let react = inv.deps.iter().find(|d| d.name == "react").unwrap();
    assert_eq!(react.ecosystem, "npm");
    assert_eq!(react.spec, "^18.0.0");
    assert_eq!(react.locked.as_deref(), Some("18.2.0"));
    let jest = inv.deps.iter().find(|d| d.name == "jest").unwrap();
    assert_eq!(jest.locked.as_deref(), Some("29.7.0"));
}

/// Why: Cargo deps (string and table form) must resolve from Cargo.lock.
/// What: writes Cargo.toml + Cargo.lock; asserts both dep forms + locked.
/// Test: this test itself.
#[test]
fn cargo_manifest_and_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"x\"\n[dependencies]\nserde = \"1.0\"\ntokio = { version = \"1\", features = [\"full\"] }\n",
    );
    write(
        tmp.path(),
        "Cargo.lock",
        "[[package]]\nname = \"serde\"\nversion = \"1.0.203\"\n\n[[package]]\nname = \"tokio\"\nversion = \"1.38.0\"\n",
    );
    let inv = build_inventory(tmp.path());
    let serde = inv.deps.iter().find(|d| d.name == "serde").unwrap();
    assert_eq!(serde.ecosystem, "cargo");
    assert_eq!(serde.spec, "1.0");
    assert_eq!(serde.locked.as_deref(), Some("1.0.203"));
    let tokio = inv.deps.iter().find(|d| d.name == "tokio").unwrap();
    assert_eq!(tokio.spec, "1");
    assert_eq!(tokio.locked.as_deref(), Some("1.38.0"));
}

/// Why: PEP 621 and poetry dependency shapes must both parse.
/// What: a pyproject with both a project array and a poetry table.
/// Test: this test itself.
#[test]
fn pyproject_pep621_and_poetry() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "pyproject.toml",
        "[project]\nname = \"x\"\ndependencies = [\"requests>=2.0\", \"httpx[cli]\"]\n\n[tool.poetry.dependencies]\npython = \"^3.11\"\nflask = \"^3.0\"\n",
    );
    let inv = build_inventory(tmp.path());
    let req = inv.deps.iter().find(|d| d.name == "requests").unwrap();
    assert_eq!(req.ecosystem, "pypi");
    assert_eq!(req.spec, ">=2.0");
    assert!(inv.deps.iter().any(|d| d.name == "httpx"));
    assert!(inv.deps.iter().any(|d| d.name == "flask"));
    assert!(
        !inv.deps.iter().any(|d| d.name == "python"),
        "the python constraint is not a dependency row"
    );
}

/// Why: go.mod pins exactly, so the declared version is also the locked one.
/// What: a go.mod with a require block and a single-line require.
/// Test: this test itself.
#[test]
fn go_mod_requires() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "go.mod",
        "module example.com/x\n\ngo 1.21\n\nrequire (\n\tgithub.com/pkg/errors v0.9.1\n)\n\nrequire golang.org/x/sync v0.7.0\n",
    );
    let inv = build_inventory(tmp.path());
    let errs = inv
        .deps
        .iter()
        .find(|d| d.name == "github.com/pkg/errors")
        .unwrap();
    assert_eq!(errs.ecosystem, "go");
    assert_eq!(errs.locked.as_deref(), Some("v0.9.1"));
    assert!(inv.deps.iter().any(|d| d.name == "golang.org/x/sync"));
}

/// Why: the report caps rows and must report the overflow count honestly.
/// What: 35 cargo deps → 30 rows + overflow 5.
/// Test: this test itself.
#[test]
fn caps_rows_with_overflow_count() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut toml = String::from("[package]\nname = \"x\"\n[dependencies]\n");
    for i in 0..35 {
        toml.push_str(&format!("dep{i:02} = \"1.0\"\n"));
    }
    write(tmp.path(), "Cargo.toml", &toml);
    let inv = build_inventory(tmp.path());
    assert_eq!(inv.total, 35);
    assert_eq!(inv.deps.len(), MAX_ROWS);
    assert_eq!(inv.overflow(), 35 - MAX_ROWS);
}

/// Why: an absent/malformed manifest must contribute nothing, never error.
/// What: an empty dir and a garbage Cargo.toml both yield an empty inventory.
/// Test: this test itself.
#[test]
fn absent_and_malformed_are_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(build_inventory(tmp.path()).is_empty());
    write(tmp.path(), "Cargo.toml", "this is not valid toml {{{");
    assert!(build_inventory(tmp.path()).is_empty());
}

/// Why: several ecosystems in one repo merge into one stable-sorted inventory.
/// What: npm + cargo present; both appear, sorted by (ecosystem, name).
/// Test: this test itself.
#[test]
fn multi_ecosystem_inventory() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"dependencies": {"react": "^18"}}"#,
    );
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"x\"\n[dependencies]\nserde = \"1\"\n",
    );
    let inv = build_inventory(tmp.path());
    assert_eq!(inv.total, 2);
    // cargo sorts before npm.
    assert_eq!(inv.deps[0].ecosystem, "cargo");
    assert_eq!(inv.deps[1].ecosystem, "npm");
}
