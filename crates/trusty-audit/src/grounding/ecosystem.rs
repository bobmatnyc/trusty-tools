//! Which dependency ecosystem a checkout declares, and what to call it (#6076).
//!
//! Why: every collector in this module that scans a dependency set owes the
//! same sentence when it cannot — the report must name the LANGUAGE it did not
//! cover, because "no cargo-deny-equivalent for Go" is actionable and "no
//! go.mod handler" is not. #6075's CVE leg established that table; #6076's
//! license leg needs the identical one, and a second copy would drift the
//! moment either grew a marker (CLAUDE.md's common-entry-point rule).
//!
//! What: [`ECOSYSTEMS`], the marker-file table, and [`detect`], which returns
//! the first language whose marker sits at the checkout root. Each collector
//! words its own gap sentence around that name — the wording differs per tool,
//! the detection does not.
//!
//! Test: `super::license::license_tests::{a_non_rust_repository_names_its_language,
//! a_repository_with_no_manifest_is_a_declared_skip}`, and the same pair in
//! `super::cve::cve_tests`.

use std::path::Path;

/// Marker files that identify a dependency ecosystem, and what to call it.
///
/// Ordered most-specific first so a polyglot repository is named by the
/// ecosystem whose manifest sits at its root. Rust is deliberately absent: a
/// collector that handles Rust checks its own `Cargo.toml` / `Cargo.lock` pair
/// before consulting this table, and owes a different sentence when it finds
/// one.
pub const ECOSYSTEMS: &[(&str, &str)] = &[
    ("package.json", "JavaScript/TypeScript"),
    ("go.mod", "Go"),
    ("pyproject.toml", "Python"),
    ("requirements.txt", "Python"),
    ("Pipfile", "Python"),
    ("Gemfile", "Ruby"),
    ("composer.json", "PHP"),
    ("pom.xml", "Java"),
    ("build.gradle", "Java/Kotlin"),
    ("build.gradle.kts", "Java/Kotlin"),
    ("mix.exs", "Elixir"),
    ("pubspec.yaml", "Dart/Flutter"),
    ("Package.swift", "Swift"),
];

/// The non-Rust ecosystem `checkout` declares at its root, if any.
///
/// Why: `None` is what separates a DEGRADATION from a declared skip. A
/// repository declaring no dependency manifest of any kind this table
/// recognises has no dependency surface a collector can claim to have missed,
/// so it earns silence rather than a gap line restating that.
/// What: one `Path::is_file` per [`ECOSYSTEMS`] row, first match wins. Costs
/// nothing beyond those stats — no subprocess and no directory walk.
///
/// # Postconditions
/// Never panics; an unreadable or absent `checkout` yields `None`.
///
/// Test: `super::license::license_tests::a_non_rust_repository_names_its_language`.
#[must_use]
pub fn detect(checkout: &Path) -> Option<&'static str> {
    ECOSYSTEMS
        .iter()
        .find(|(marker, _)| checkout.join(marker).is_file())
        .map(|(_, language)| *language)
}
