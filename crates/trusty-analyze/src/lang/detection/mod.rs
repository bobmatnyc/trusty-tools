//! File-extension-based language and build-system detection.
//!
//! Why: Before invoking a `LanguageAnalyzer`, we need to know which one to
//! pick. This module provides cheap path-string heuristics that work without
//! reading file contents.
//!
//! What: `LanguageDetector::detect_file` maps an extension to a language
//! string; `LanguageDetector::detect` aggregates over a slice of paths and
//! also recognizes build manifests (Cargo.toml, package.json, etc.).
//! Manifest-content-based framework inference lives in the [`frameworks`]
//! submodule and is re-exported as [`detect_frameworks`].
//!
//! Test: `detect_file_extension_mapping` covers each supported extension;
//! `detect_picks_primary_language` ensures the most common extension wins.

use std::collections::HashMap;

mod frameworks;
pub use frameworks::detect_frameworks;

/// Detected language(s) for a repository or set of files.
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// Most common language by file count.
    pub primary_language: String,
    /// All detected languages (deduplicated).
    pub languages: Vec<String>,
    /// `"cargo"`, `"maven"`, `"gradle"`, `"npm"`, `"pip"`, `"go-mod"`, ...
    pub build_system: Option<String>,
    /// Fraction of files that matched a known extension.
    pub confidence: f32,
    /// Frameworks detected from manifest files (e.g. `"Next.js"`, `"Django"`,
    /// `"Rails"`). Populated by [`detect_frameworks`] when a project root is
    /// available; empty when only a flat file list is given.
    pub frameworks: Vec<String>,
}

/// File-extension-based language detector.
pub struct LanguageDetector;

/// Per-language extension matchers. Each helper returns the canonical
/// language tag if the path's extension belongs to that language.
///
/// Why: Splitting per-language keeps each helper trivially testable and
/// caps the cyclomatic complexity of the dispatcher at the number of
/// supported languages, regardless of how many extensions each one has.
/// What: Lowercase suffix match against a language's known extension set.
/// Test: `detect_file_extension_mapping` exercises each helper through the
/// public `detect_file` dispatcher.
fn detect_rust(lower: &str) -> Option<&'static str> {
    if lower.ends_with(".rs") {
        Some("rust")
    } else {
        None
    }
}

fn detect_typescript(lower: &str) -> Option<&'static str> {
    if lower.ends_with(".tsx") || lower.ends_with(".ts") {
        Some("typescript")
    } else {
        None
    }
}

fn detect_javascript(lower: &str) -> Option<&'static str> {
    const EXTS: &[&str] = &[".jsx", ".js", ".mjs", ".cjs"];
    if EXTS.iter().any(|e| lower.ends_with(e)) {
        Some("javascript")
    } else {
        None
    }
}

fn detect_python(lower: &str) -> Option<&'static str> {
    if lower.ends_with(".py") || lower.ends_with(".pyi") {
        Some("python")
    } else {
        None
    }
}

fn detect_java(lower: &str) -> Option<&'static str> {
    if lower.ends_with(".java") {
        Some("java")
    } else {
        None
    }
}

fn detect_go(lower: &str) -> Option<&'static str> {
    if lower.ends_with(".go") {
        Some("go")
    } else {
        None
    }
}

fn detect_cpp(lower: &str) -> Option<&'static str> {
    const EXTS: &[&str] = &[".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".c", ".h"];
    if EXTS.iter().any(|e| lower.ends_with(e)) {
        Some("cpp")
    } else {
        None
    }
}

fn detect_kotlin(lower: &str) -> Option<&'static str> {
    (lower.ends_with(".kt") || lower.ends_with(".kts")).then_some("kotlin")
}

fn detect_swift(lower: &str) -> Option<&'static str> {
    lower.ends_with(".swift").then_some("swift")
}

fn detect_ruby(lower: &str) -> Option<&'static str> {
    lower.ends_with(".rb").then_some("ruby")
}

fn detect_php(lower: &str) -> Option<&'static str> {
    lower.ends_with(".php").then_some("php")
}

/// Ordered list of per-language detectors. First match wins.
///
/// Why: `run_diagnostics` gates every file on `detect_file` before dispatching
/// to its linters, so any language missing here is silently skipped even when
/// its linter is installed (issue #963). The tags returned must match the
/// `StaticTool::language()` bucket keys (`kotlin`/`swift`/`ruby`/`php`) and the
/// tree-sitter adapter self-reports, or routed files reach no tool.
type LanguageDetectorFn = fn(&str) -> Option<&'static str>;
const LANGUAGE_DETECTORS: &[LanguageDetectorFn] = &[
    detect_rust,
    detect_typescript,
    detect_javascript,
    detect_python,
    detect_java,
    detect_go,
    detect_cpp,
    detect_kotlin,
    detect_swift,
    detect_ruby,
    detect_php,
];

/// Per-build-system manifest matchers. Each helper returns the canonical
/// build-system tag if the path basename matches a known manifest.
fn build_cargo(lower: &str) -> Option<&'static str> {
    matches_basename(lower, &["cargo.toml"]).then_some("cargo")
}

fn build_maven(lower: &str) -> Option<&'static str> {
    matches_basename(lower, &["pom.xml"]).then_some("maven")
}

fn build_gradle(lower: &str) -> Option<&'static str> {
    matches_basename(lower, &["build.gradle", "build.gradle.kts"]).then_some("gradle")
}

fn build_npm(lower: &str) -> Option<&'static str> {
    matches_basename(lower, &["package.json"]).then_some("npm")
}

fn build_pip(lower: &str) -> Option<&'static str> {
    matches_basename(lower, &["pyproject.toml", "setup.py", "requirements.txt"]).then_some("pip")
}

fn build_go_mod(lower: &str) -> Option<&'static str> {
    matches_basename(lower, &["go.mod"]).then_some("go-mod")
}

const BUILD_SYSTEM_DETECTORS: &[LanguageDetectorFn] = &[
    build_cargo,
    build_maven,
    build_gradle,
    build_npm,
    build_pip,
    build_go_mod,
];

/// True if `lower` equals any of `names` or ends with `/` + name.
fn matches_basename(lower: &str, names: &[&str]) -> bool {
    names
        .iter()
        .any(|n| lower == *n || lower.ends_with(&format!("/{n}")))
}

impl LanguageDetector {
    /// Detect the language of a single file from its extension.
    /// Returns `None` for unknown extensions.
    pub fn detect_file(path: &str) -> Option<String> {
        let lower = path.to_lowercase();
        LANGUAGE_DETECTORS
            .iter()
            .find_map(|d| d(&lower))
            .map(|s| s.to_string())
    }

    /// Detect a build system from a single file basename.
    fn detect_build_system_for(path: &str) -> Option<&'static str> {
        let lower = path.to_lowercase();
        BUILD_SYSTEM_DETECTORS.iter().find_map(|d| d(&lower))
    }

    /// Detect languages from a list of file paths. Returns the primary
    /// language (most common matching extension), all detected languages,
    /// the most authoritative build system found, and a confidence score
    /// equal to the fraction of files that matched a known extension.
    pub fn detect(files: &[&str]) -> DetectionResult {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut build: Option<&'static str> = None;
        let total = files.len().max(1);
        let mut matched = 0usize;

        for f in files {
            if let Some(lang) = Self::detect_file(f) {
                *counts.entry(lang).or_insert(0) += 1;
                matched += 1;
            }
            if build.is_none() {
                if let Some(bs) = Self::detect_build_system_for(f) {
                    build = Some(bs);
                }
            }
        }

        let mut langs: Vec<(String, usize)> = counts.into_iter().collect();
        langs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let primary = langs
            .first()
            .map(|(l, _)| l.clone())
            .unwrap_or_else(|| "unknown".into());

        let all = langs.iter().map(|(l, _)| l.clone()).collect();

        DetectionResult {
            primary_language: primary,
            languages: all,
            build_system: build.map(|s| s.to_string()),
            confidence: matched as f32 / total as f32,
            frameworks: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_file_extension_mapping() {
        assert_eq!(
            LanguageDetector::detect_file("src/main.rs"),
            Some("rust".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("App.tsx"),
            Some("typescript".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("foo.ts"),
            Some("typescript".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("foo.js"),
            Some("javascript".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("module.mjs"),
            Some("javascript".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("script.py"),
            Some("python".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("Foo.java"),
            Some("java".into())
        );
        assert_eq!(LanguageDetector::detect_file("main.go"), Some("go".into()));
        assert_eq!(
            LanguageDetector::detect_file("Main.kt"),
            Some("kotlin".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("build.gradle.kts"),
            Some("kotlin".into())
        );
        assert_eq!(
            LanguageDetector::detect_file("App.swift"),
            Some("swift".into())
        );
        assert_eq!(LanguageDetector::detect_file("app.rb"), Some("ruby".into()));
        assert_eq!(
            LanguageDetector::detect_file("index.php"),
            Some("php".into())
        );
        assert_eq!(LanguageDetector::detect_file("README.md"), None);
    }

    #[test]
    fn detect_file_returns_linter_bucket_keys_for_kotlin_swift_ruby_php() {
        // Regression for #963: these tags must match the `StaticTool::language()`
        // bucket keys so `run_diagnostics` can route .kt/.swift/.rb/.php files
        // to detekt/swiftlint/rubocop/phpstan instead of dropping them.
        assert_eq!(detect_kotlin("foo.kt"), Some("kotlin"));
        assert_eq!(detect_kotlin("foo.kts"), Some("kotlin"));
        assert_eq!(detect_kotlin("foo.java"), None);
        assert_eq!(detect_swift("foo.swift"), Some("swift"));
        assert_eq!(detect_swift("foo.kt"), None);
        assert_eq!(detect_ruby("foo.rb"), Some("ruby"));
        assert_eq!(detect_ruby("foo.rs"), None);
        assert_eq!(detect_php("foo.php"), Some("php"));
        assert_eq!(detect_php("foo.py"), None);
    }

    #[test]
    fn detect_picks_primary_language() {
        let files = ["a.rs", "b.rs", "c.rs", "d.ts", "Cargo.toml"];
        let r = LanguageDetector::detect(&files);
        assert_eq!(r.primary_language, "rust");
        assert!(r.languages.contains(&"rust".to_string()));
        assert!(r.languages.contains(&"typescript".to_string()));
        assert_eq!(r.build_system.as_deref(), Some("cargo"));
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn detect_recognizes_npm_and_python() {
        let files = ["a.ts", "package.json", "tsconfig.json"];
        let r = LanguageDetector::detect(&files);
        assert_eq!(r.build_system.as_deref(), Some("npm"));

        let files = ["main.py", "pyproject.toml"];
        let r = LanguageDetector::detect(&files);
        assert_eq!(r.primary_language, "python");
        assert_eq!(r.build_system.as_deref(), Some("pip"));
    }

    // --- Helper-level unit tests (new, per refactor target) ------------------

    #[test]
    fn detect_rust_helper_matches_only_dot_rs() {
        assert_eq!(detect_rust("foo.rs"), Some("rust"));
        assert_eq!(detect_rust("foo.RS"), None); // detect_file lowercases first
        assert_eq!(detect_rust("foo.rust"), None);
        assert_eq!(detect_rust("foo.py"), None);
    }

    #[test]
    fn detect_javascript_helper_covers_all_js_variants() {
        assert_eq!(detect_javascript("a.js"), Some("javascript"));
        assert_eq!(detect_javascript("a.jsx"), Some("javascript"));
        assert_eq!(detect_javascript("a.mjs"), Some("javascript"));
        assert_eq!(detect_javascript("a.cjs"), Some("javascript"));
        assert_eq!(detect_javascript("a.ts"), None);
    }

    #[test]
    fn detect_cpp_helper_covers_c_and_cpp_extensions() {
        for ext in [".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".c", ".h"] {
            let path = format!("file{ext}");
            assert_eq!(detect_cpp(&path), Some("cpp"), "ext={ext}");
        }
        assert_eq!(detect_cpp("file.txt"), None);
    }

    #[test]
    fn matches_basename_handles_root_and_nested() {
        assert!(matches_basename("cargo.toml", &["cargo.toml"]));
        assert!(matches_basename("a/b/cargo.toml", &["cargo.toml"]));
        assert!(!matches_basename("cargo.toml.bak", &["cargo.toml"]));
        assert!(!matches_basename("notcargo.toml", &["cargo.toml"]));
    }

    #[test]
    fn build_system_helpers_are_independent() {
        assert_eq!(build_cargo("cargo.toml"), Some("cargo"));
        assert_eq!(build_maven("pom.xml"), Some("maven"));
        assert_eq!(build_gradle("build.gradle.kts"), Some("gradle"));
        assert_eq!(build_npm("a/package.json"), Some("npm"));
        assert_eq!(build_pip("requirements.txt"), Some("pip"));
        assert_eq!(build_go_mod("go.mod"), Some("go-mod"));
        assert_eq!(build_cargo("pom.xml"), None);
    }
}
