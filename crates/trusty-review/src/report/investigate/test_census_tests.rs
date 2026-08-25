//! Tests for the deterministic test-coverage enumeration (#6193).

use super::*;

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

/// The count is over the WHOLE tracked list, not a sample of it — which is the
/// property that makes it reproducible across runs with different budgets.
#[test]
fn counts_test_files_over_the_whole_tracked_list() {
    let c = census(&paths(&[
        "Cargo.toml",
        "src/lib.rs",
        "src/main.rs",
        "tests/api.rs",
        "tests/cli.rs",
        "src/thing_tests.rs",
    ]));
    assert_eq!(c.test_files, 3);
    assert_eq!(c.packages_total, 1);
    assert_eq!(c.packages_with_tests, 1);
    assert!(c.packages_without_tests.is_empty());
}

/// A test under a nested package counts for that package, not for the workspace
/// root above it — otherwise one tested crate would report the whole workspace
/// as tested.
#[test]
fn attributes_a_test_to_its_nearest_package() {
    let c = census(&paths(&[
        "Cargo.toml",
        "crates/a/Cargo.toml",
        "crates/a/src/lib.rs",
        "crates/a/tests/it.rs",
        "crates/b/Cargo.toml",
        "crates/b/src/lib.rs",
    ]));
    assert_eq!(c.packages_total, 3, "{c:?}");
    assert_eq!(c.packages_with_tests, 1, "{c:?}");
    assert_eq!(c.packages_without_tests, vec![".", "crates/b"]);
}

/// A repository declaring no manifest is one package, named `.` — never zero
/// packages, which would render as "0 of 0 carry tests".
#[test]
fn a_repository_with_no_manifest_is_one_package() {
    let c = census(&paths(&["README.md", "main.py"]));
    assert_eq!(c.packages_total, 1);
    assert_eq!(c.packages_with_tests, 0);
    assert_eq!(c.packages_without_tests, vec!["."]);
    assert_eq!(c.test_files, 0);
}

/// Non-Cargo ecosystems are package roots too.
#[test]
fn other_ecosystems_declare_packages() {
    let c = census(&paths(&[
        "web/package.json",
        "web/src/index.ts",
        "web/src/index.test.ts",
        "api/pyproject.toml",
        "api/app.py",
    ]));
    assert_eq!(c.packages_total, 2, "{c:?}");
    assert_eq!(c.packages_with_tests, 1, "{c:?}");
    assert_eq!(c.packages_without_tests, vec!["api"]);
}

/// The row must say it was enumerated. A bare count next to five sampled counts
/// reads as a sixth sampled count.
#[test]
fn the_line_states_the_enumeration_basis() {
    let c = census(&paths(&["Cargo.toml", "src/lib.rs", "tests/it.rs"]));
    let line = c.line();
    assert!(line.contains("1 test file(s) enumerated"), "{line}");
    assert!(line.contains("not sampled"), "{line}");
    assert!(line.contains("1 of 1 package(s) carry tests"), "{line}");
}

/// Untested packages are named, and a long list states the remainder as a count
/// rather than filling the bullet.
#[test]
fn the_line_names_untested_packages() {
    let mut list = vec!["Cargo.toml".to_string()];
    for i in 0..8 {
        list.push(format!("crates/p{i}/Cargo.toml"));
        list.push(format!("crates/p{i}/src/lib.rs"));
    }
    let refs: Vec<&str> = list.iter().map(String::as_str).collect();
    let c = census(&paths(&refs));
    let line = c.line();
    assert!(line.contains("without tests:"), "{line}");
    assert!(line.contains("crates/p0"), "{line}");
    assert!(line.contains("and 4 more"), "{line}");
}

/// Two runs over the same list agree exactly — the reproducibility the issue's
/// closure condition names.
#[test]
fn the_census_is_reproducible() {
    let files = paths(&["Cargo.toml", "src/lib.rs", "tests/a.rs", "tests/b.rs"]);
    assert_eq!(census(&files), census(&files));
}
