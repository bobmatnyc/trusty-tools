//! Deterministic test-coverage enumeration over the tracked file list (#6193).
//!
//! Why: the "test coverage" row of the investigation coverage section counted
//! EXAMINED files — the ones the byte budget let the LLM read. Two runs over an
//! unchanged repository with a different budget therefore reported different
//! test-coverage figures, and an external engagement user read the dimension as
//! under-grounded because of it. Test presence is a repo-level fact: it is
//! knowable from the tracked file list alone, at no I/O cost, and it does not
//! move when the sample does.
//!
//! What: [`TestCensus`] counts test files and per-package test presence over the
//! full tracked list; [`census`] computes it. The LLM still narrates the
//! dimension — it no longer supplies its counts.
//!
//! Scope: presence, not adequacy. Nothing here reads a test file, counts
//! assertions, or claims a coverage percentage; a package with one smoke test
//! counts as carrying tests, and the row says exactly that.
//!
//! Test: `test_census_tests.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// The manifest filenames that mark a directory as a package root.
///
/// One entry per ecosystem the DD dimensions already name elsewhere. A
/// repository with none of them has one implicit package — the repository — and
/// the census says so rather than reporting zero packages.
const PACKAGE_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
];

/// How many package names the census names when they carry no tests.
///
/// A due-diligence reader needs the untested packages by name; a list of forty
/// is noise in a bullet, so the row names the first few and states the rest as a
/// count.
const MAX_NAMED: usize = 5;

/// One repository's test presence, enumerated rather than sampled (#6193).
///
/// Why: see the module doc. Every field is derived from the tracked file list,
/// so two runs over an unchanged checkout produce identical values whatever the
/// investigation budget was.
/// What: `test_files` counts tracked paths that look like tests;
/// `packages_total` counts the package roots found (at least 1);
/// `packages_with_tests` counts those with at least one test path beneath them;
/// `packages_without_tests` names the rest, in path order.
/// Test: `test_census_tests::{counts_test_files_over_the_whole_tracked_list,
/// attributes_a_test_to_its_nearest_package}`.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct TestCensus {
    /// Tracked paths recognised as test files.
    pub test_files: usize,
    /// Package roots found, or 1 for a repository declaring no manifest.
    pub packages_total: usize,
    /// Package roots with at least one test path beneath them.
    pub packages_with_tests: usize,
    /// The package roots carrying no test path, in path order.
    pub packages_without_tests: Vec<String>,
}

impl TestCensus {
    /// The coverage-section line for the `test coverage` dimension.
    ///
    /// Why: the row must state its own basis. A reader who sees a count next to
    /// five sampled-file counts will read it as a sixth sampled count unless the
    /// line says otherwise.
    /// What: the test-file total, the package split, and up to [`MAX_NAMED`]
    /// untested package names with the remainder as a count.
    /// Test: `test_census_tests::{the_line_states_the_enumeration_basis,
    /// the_line_names_untested_packages}`.
    pub fn line(&self) -> String {
        let mut out = format!(
            "{} test file(s) enumerated across the whole tracked list (not sampled); {} of {} \
             package(s) carry tests",
            self.test_files, self.packages_with_tests, self.packages_total,
        );
        if !self.packages_without_tests.is_empty() {
            let named: Vec<&str> = self
                .packages_without_tests
                .iter()
                .take(MAX_NAMED)
                .map(String::as_str)
                .collect();
            out.push_str(&format!(" — without tests: {}", named.join(", ")));
            let rest = self
                .packages_without_tests
                .len()
                .saturating_sub(named.len());
            if rest > 0 {
                out.push_str(&format!(", and {rest} more"));
            }
        }
        out
    }
}

/// Enumerate `files` for test presence.
///
/// Why/What: see the module doc and [`TestCensus`]. Package roots come from
/// [`PACKAGE_MANIFESTS`]; every other path is attributed to the deepest package
/// root that is a prefix of it, so a test under `crates/a/tests/` counts for
/// `crates/a` and not for the workspace root above it. A repository declaring no
/// manifest is one package named `.`.
/// Test: `test_census_tests::{counts_test_files_over_the_whole_tracked_list,
/// attributes_a_test_to_its_nearest_package,
/// a_repository_with_no_manifest_is_one_package}`.
pub fn census(files: &[PathBuf]) -> TestCensus {
    let paths: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().replace('\\', "/"))
        .collect();

    // Package roots: the directory holding a recognised manifest. `""` is the
    // repository root, rendered as `.`.
    let mut packages: BTreeMap<String, bool> = BTreeMap::new();
    for p in &paths {
        let base = p.rsplit('/').next().unwrap_or(p);
        if PACKAGE_MANIFESTS.contains(&base) {
            let dir = p
                .rsplit_once('/')
                .map_or(String::new(), |(d, _)| d.to_string());
            packages.entry(dir).or_insert(false);
        }
    }
    if packages.is_empty() {
        packages.insert(String::new(), false);
    }

    let mut test_files = 0usize;
    for p in &paths {
        if !super::select::is_test_path(p) {
            continue;
        }
        test_files += 1;
        if let Some(owner) = owning_package(p, &packages) {
            packages.insert(owner, true);
        }
    }

    let packages_without_tests: Vec<String> = packages
        .iter()
        .filter(|(_, has)| !**has)
        .map(|(dir, _)| display_package(dir))
        .collect();
    TestCensus {
        packages_total: packages.len(),
        packages_with_tests: packages.len() - packages_without_tests.len(),
        packages_without_tests,
        test_files,
    }
}

/// The deepest package root that is a directory prefix of `path`.
fn owning_package(path: &str, packages: &BTreeMap<String, bool>) -> Option<String> {
    packages
        .keys()
        .filter(|dir| under(path, dir))
        .max_by_key(|dir| dir.len())
        .cloned()
}

/// True when `path` sits at or beneath the package directory `dir`.
fn under(path: &str, dir: &str) -> bool {
    if dir.is_empty() {
        return true;
    }
    path.starts_with(dir) && path.as_bytes().get(dir.len()) == Some(&b'/')
}

/// The repository root renders as `.`; every other package as its own directory.
fn display_package(dir: &str) -> String {
    if dir.is_empty() {
        ".".to_string()
    } else {
        dir.to_string()
    }
}

#[cfg(test)]
#[path = "test_census_tests.rs"]
mod tests;
