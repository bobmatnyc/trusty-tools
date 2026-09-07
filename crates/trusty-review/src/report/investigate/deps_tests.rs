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

/// Write a Cargo.toml at `root` declaring `count` dependencies, `dep00`-up.
fn cargo_manifest_with(root: &Path, count: usize) {
    let mut toml = String::from("[package]\nname = \"x\"\n[dependencies]\n");
    for i in 0..count {
        toml.push_str(&format!("dep{i:02} = \"1.0\"\n"));
    }
    write(root, "Cargo.toml", &toml);
}

/// Why: the report caps rows and must report the overflow count honestly.
/// What: 35 cargo deps → 30 rendered rows + overflow 5.
/// Test: this test itself.
#[test]
fn rendered_caps_at_max_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    cargo_manifest_with(tmp.path(), 35);
    let inv = build_inventory(tmp.path());
    assert_eq!(inv.total, 35);
    assert_eq!(inv.rendered().len(), MAX_ROWS);
    assert_eq!(inv.overflow(), 35 - MAX_ROWS);
}

/// Why: #6788 — `build_inventory` truncated to [`MAX_ROWS`] before the
/// inventory was serialised into `investigation.json`, so trusty-audit's OSV
/// lookup saw 30 packages per repository however many the manifest declared.
/// What: 35 cargo deps → every row is kept in `deps` and survives a serde
/// round-trip, in order, while `total` still reports 35.
/// Test: this test itself.
#[test]
fn inventory_keeps_every_row_past_the_render_cap() {
    let tmp = tempfile::TempDir::new().unwrap();
    cargo_manifest_with(tmp.path(), 35);
    let inv = build_inventory(tmp.path());

    assert_eq!(inv.total, 35);
    assert_eq!(inv.deps.len(), 35, "the inventory must not be truncated");

    let json = serde_json::to_value(&inv).expect("inventory serialises");
    let rows = json["deps"].as_array().expect("deps array");
    assert_eq!(rows.len(), 35, "every row must reach the snapshot");
    assert_eq!(rows[34]["name"], "dep34", "the last row is the 35th");
    assert_eq!(json["total"], 35);
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

/// #6137: a cargo WORKSPACE root declares its shared dependencies under
/// `[workspace.dependencies]` only. Reading `[dependencies]` alone reported
/// zero for a 134-dependency workspace, which the section then stated as a
/// clean result.
#[test]
fn workspace_dependencies_are_inventoried() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\n\
         serde = \"1\"\ntokio = { version = \"1.40\", features = [\"full\"] }\n",
    );
    let inv = build_inventory(tmp.path());
    assert_eq!(inv.total, 2, "inv: {:?}", inv.deps);
    assert!(inv.deps.iter().any(|d| d.name == "serde"));
    assert!(
        inv.deps
            .iter()
            .any(|d| d.name == "tokio" && d.spec == "1.40")
    );
}

/// #6137: "the manifests declare nothing" and "no manifest was examined" must
/// be distinguishable at the render layer, so the probe records what it read.
#[test]
fn records_the_manifests_it_examined() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(build_inventory(tmp.path()).manifests_examined.is_empty());

    write(tmp.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
    let inv = build_inventory(tmp.path());
    assert_eq!(inv.total, 0, "an empty workspace declares nothing");
    assert_eq!(inv.manifests_examined, vec!["Cargo.toml".to_string()]);
}

/// One dependency's Locked cell, or the empty string when it has none.
fn locked_cell(inv: &DependencyInventory, name: &str) -> String {
    inv.deps
        .iter()
        .find(|d| d.name == name)
        .and_then(|d| d.locked.clone())
        .unwrap_or_default()
}

/// #6080: the graded defect, both cases verbatim.
///
/// Why: a workspace lockfile carries several versions of one crate because
/// transitive dependents pin older majors. First-wins reported the LOWEST —
/// `base64` declared `0.22` rendered as `0.13.1`, `dashmap` declared `6` as
/// `5.5.3` — so the Locked column named a version the declared requirement
/// cannot resolve to.
/// What: with both versions in the lock, each row shows the one satisfying its
/// declared requirement.
#[test]
fn cargo_locked_version_satisfies_the_declared_req() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[workspace.dependencies]\nbase64 = \"0.22\"\ndashmap = \"6\"\n",
    );
    write(
        tmp.path(),
        "Cargo.lock",
        "[[package]]\nname = \"base64\"\nversion = \"0.13.1\"\n\n\
         [[package]]\nname = \"base64\"\nversion = \"0.22.1\"\n\n\
         [[package]]\nname = \"dashmap\"\nversion = \"5.5.3\"\n\n\
         [[package]]\nname = \"dashmap\"\nversion = \"6.1.0\"\n",
    );
    let inv = build_inventory(tmp.path());
    assert_eq!(locked_cell(&inv, "base64"), "0.22.1");
    assert_eq!(locked_cell(&inv, "dashmap"), "6.1.0");
}

/// Why/What (#6080): with several locked versions satisfying one requirement,
/// the build resolves to the highest, so that is what the row states.
#[test]
fn cargo_locked_prefers_the_highest_satisfying_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(tmp.path(), "Cargo.toml", "[dependencies]\nserde = \"1\"\n");
    write(
        tmp.path(),
        "Cargo.lock",
        "[[package]]\nname = \"serde\"\nversion = \"1.0.100\"\n\n\
         [[package]]\nname = \"serde\"\nversion = \"1.0.219\"\n",
    );
    let inv = build_inventory(tmp.path());
    assert_eq!(locked_cell(&inv, "serde"), "1.0.219");
}

/// Why (#6080): naming an unrelated version as the resolution is the defect
/// this fix exists to stop, so a requirement nothing satisfies says exactly
/// that rather than picking one anyway.
/// What: the cell names every candidate and states that none matches.
#[test]
fn cargo_locked_states_when_no_version_satisfies() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(tmp.path(), "Cargo.toml", "[dependencies]\nfoo = \"3\"\n");
    write(
        tmp.path(),
        "Cargo.lock",
        "[[package]]\nname = \"foo\"\nversion = \"1.0.0\"\n\n\
         [[package]]\nname = \"foo\"\nversion = \"2.0.0\"\n",
    );
    let inv = build_inventory(tmp.path());
    let locked = locked_cell(&inv, "foo");
    assert!(locked.contains("none satisfies 3"), "locked: {locked}");
    assert!(locked.contains("1.0.0"), "locked: {locked}");
    assert!(locked.contains("2.0.0"), "locked: {locked}");
    // #6794: the cell holds prose, not a version, so the row is not resolved.
    let foo = inv.deps.iter().find(|d| d.name == "foo").unwrap();
    assert!(
        !foo.resolved,
        "a no-match note is not a resolution: {foo:?}"
    );
}

/// One dependency row by name, for the #6794 assertions below.
fn row<'a>(inv: &'a DependencyInventory, name: &str) -> &'a Dependency {
    inv.deps
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no `{name}` row in {:?}", inv.deps))
}

/// #6794: the graded case. A declared range plus a lockfile must record the
/// resolved version and the file it came from; the same manifest with no
/// lockfile must record the range and say it is unresolved.
///
/// Why: `serde = "1"` and `serde 1.0.210` reached trusty-audit's OSV stage
/// indistinguishable, and a range is unscannable — 515 of 1230 rows in a
/// 59-repository run.
#[test]
fn cargo_lock_records_its_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"x\"\n[dependencies]\nserde = \"1\"\n",
    );
    write(
        tmp.path(),
        "Cargo.lock",
        "[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\n",
    );
    let serde = row(&build_inventory(tmp.path()), "serde").clone();
    assert_eq!(serde.spec, "1");
    assert_eq!(serde.locked.as_deref(), Some("1.0.210"));
    assert!(serde.resolved, "{serde:?}");
    assert_eq!(serde.source.as_deref(), Some("Cargo.lock"));

    std::fs::remove_file(tmp.path().join("Cargo.lock")).unwrap();
    let inv = build_inventory(tmp.path());
    let serde = row(&inv, "serde");
    assert_eq!(serde.spec, "1", "the declared range is still recorded");
    assert_eq!(serde.locked, None);
    assert!(!serde.resolved, "{serde:?}");
    assert_eq!(serde.source, None);
    assert_eq!(inv.resolved_count(), 0);
}

/// #6794: the same contract for the JS ecosystem — `package-lock.json` names
/// itself as the source, and removing it drops the row back to its range.
#[test]
fn npm_lock_records_its_source() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"dependencies": {"react": "^18.0.0"}}"#,
    );
    write(
        tmp.path(),
        "package-lock.json",
        r#"{"lockfileVersion": 3, "packages": {"": {}, "node_modules/react": {"version": "18.2.0"}}}"#,
    );
    let react = row(&build_inventory(tmp.path()), "react").clone();
    assert_eq!(react.locked.as_deref(), Some("18.2.0"));
    assert!(react.resolved, "{react:?}");
    assert_eq!(react.source.as_deref(), Some("package-lock.json"));

    std::fs::remove_file(tmp.path().join("package-lock.json")).unwrap();
    let react = row(&build_inventory(tmp.path()), "react").clone();
    assert_eq!(react.spec, "^18.0.0");
    assert_eq!(react.locked, None);
    assert!(!react.resolved, "{react:?}");
}

/// A pyproject declaring PEP 621 ranges, for the three pypi pin sources below.
fn pyproject_with_ranges(root: &Path) {
    write(
        root,
        "pyproject.toml",
        "[project]\nname = \"x\"\ndependencies = [\"requests>=2\", \"PyYAML>=6\"]\n",
    );
}

/// #6794: `pyproject.toml` states ranges only, so before this every python row
/// was unresolved — the single largest share of the unscannable rows.
#[test]
fn poetry_lock_resolves_pyproject_ranges() {
    let tmp = tempfile::TempDir::new().unwrap();
    pyproject_with_ranges(tmp.path());
    write(
        tmp.path(),
        "poetry.lock",
        "[[package]]\nname = \"requests\"\nversion = \"2.32.3\"\n\n\
         [[package]]\nname = \"pyyaml\"\nversion = \"6.0.2\"\n",
    );
    let inv = build_inventory(tmp.path());
    let requests = row(&inv, "requests");
    assert_eq!(requests.locked.as_deref(), Some("2.32.3"));
    assert!(requests.resolved, "{requests:?}");
    assert_eq!(requests.source.as_deref(), Some("poetry.lock"));
    // PyPI folds case and `-`/`_`/`.`, so `PyYAML` matches the lock's `pyyaml`.
    assert_eq!(row(&inv, "PyYAML").locked.as_deref(), Some("6.0.2"));
    assert_eq!(inv.resolved_count(), 2);
}

/// #6794: uv writes the same `[[package]]` shape under a different filename,
/// and the source column must name the file actually read.
#[test]
fn uv_lock_resolves_pyproject_ranges() {
    let tmp = tempfile::TempDir::new().unwrap();
    pyproject_with_ranges(tmp.path());
    write(
        tmp.path(),
        "uv.lock",
        "[[package]]\nname = \"requests\"\nversion = \"2.31.0\"\n",
    );
    let requests = row(&build_inventory(tmp.path()), "requests").clone();
    assert_eq!(requests.locked.as_deref(), Some("2.31.0"));
    assert_eq!(requests.source.as_deref(), Some("uv.lock"));
}

/// #6794: a `requirements.txt` `==` pin is a resolution; every other line shape
/// in it is a range or a flag and contributes nothing.
#[test]
fn requirements_pins_resolve_pyproject_ranges() {
    let tmp = tempfile::TempDir::new().unwrap();
    pyproject_with_ranges(tmp.path());
    write(
        tmp.path(),
        "requirements.txt",
        "# comment\n-r other.txt\nrequests==2.32.3  # pinned\nPyYAML>=6\n",
    );
    let inv = build_inventory(tmp.path());
    let requests = row(&inv, "requests");
    assert_eq!(requests.locked.as_deref(), Some("2.32.3"));
    assert_eq!(requests.source.as_deref(), Some("requirements.txt"));
    let pyyaml = row(&inv, "PyYAML");
    assert_eq!(pyyaml.locked, None, "a `>=` line is not a pin: {pyyaml:?}");
    assert!(!pyyaml.resolved);
}

/// #6794: a lockfile that fails to parse must be NAMED and must not stop the
/// pass — the rows fall back to their declared ranges and the run report says
/// which file could not be read.
#[test]
fn a_malformed_lockfile_warns_and_falls_back() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"x\"\n[dependencies]\nserde = \"1\"\n",
    );
    write(tmp.path(), "Cargo.lock", "[[package\nname = broken\n");
    write(
        tmp.path(),
        "package.json",
        r#"{"dependencies": {"react": "^18"}}"#,
    );
    write(tmp.path(), "package-lock.json", "{not json");

    let inv = build_inventory(tmp.path());
    assert_eq!(inv.total, 2, "the sweep completed: {:?}", inv.deps);
    assert_eq!(row(&inv, "serde").spec, "1");
    assert!(!row(&inv, "serde").resolved);
    assert!(!row(&inv, "react").resolved);
    assert_eq!(inv.resolved_count(), 0);

    assert_eq!(
        inv.lockfile_warnings.len(),
        2,
        "{:?}",
        inv.lockfile_warnings
    );
    let joined = inv.lockfile_warnings.join("\n");
    assert!(
        joined.contains("Cargo.lock was found but could not be parsed"),
        "{joined}"
    );
    assert!(
        joined.contains("package-lock.json was found but could not be parsed"),
        "{joined}"
    );
    assert!(
        joined.contains("fall back to their declared ranges"),
        "{joined}"
    );
}

/// #6794: go.mod pins exactly, so its rows are resolved and name the manifest
/// itself as the source — there is no separate lockfile to read.
#[test]
fn go_mod_rows_are_resolved_from_the_manifest() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(
        tmp.path(),
        "go.mod",
        "module x\n\nrequire github.com/pkg/errors v0.9.1\n",
    );
    let inv = build_inventory(tmp.path());
    let errors = row(&inv, "github.com/pkg/errors");
    assert!(errors.resolved, "{errors:?}");
    assert_eq!(errors.source.as_deref(), Some("go.mod"));
}
