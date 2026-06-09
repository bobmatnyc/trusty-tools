//! File-extension-based language and build-system detection.
//!
//! Why: Before invoking a `LanguageAnalyzer`, we need to know which one to
//! pick. This module provides cheap path-string heuristics that work without
//! reading file contents.
//!
//! What: `LanguageDetector::detect_file` maps an extension to a language
//! string; `LanguageDetector::detect` aggregates over a slice of paths and
//! also recognizes build manifests (Cargo.toml, package.json, etc.).
//!
//! Test: `detect_file_extension_mapping` covers each supported extension;
//! `detect_picks_primary_language` ensures the most common extension wins.

use std::collections::HashMap;
use std::path::Path;

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

type BuildDetectorFn = fn(&str) -> Option<&'static str>;
const BUILD_SYSTEM_DETECTORS: &[BuildDetectorFn] = &[
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
    ///
    /// Why: delegates to the canonical `ext_map::lang_for_linter` so all
    /// language routing goes through one table. Returns `None` for
    /// unrecognized extensions (callers can skip those files).
    /// What: returns the linter tag — e.g. `.tsx` → `"typescript"`,
    /// `.c` → `"cpp"` — so `ToolRegistry` lookups resolve correctly.
    /// Test: `detect_file_extension_mapping` exercises the full extension set.
    pub fn detect_file(path: &str) -> Option<String> {
        super::ext_map::lang_for_linter(path).map(|s| s.to_string())
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

/// Why: framework-specific conventions differ significantly from generic
/// language rules; knowing the framework allows targeted anti-pattern
/// detection (e.g. Next.js server/client component boundaries, Rails strong
/// params, Django ORM patterns).
/// What: reads manifest file contents from the project root and infers
/// framework names from declared dependencies. Returns deduplicated, sorted
/// framework name strings.
/// Test: unit tests in this module covering each manifest type (see
/// `detect_frameworks_*`).
pub fn detect_frameworks(project_root: &Path) -> Vec<String> {
    let mut found: Vec<&'static str> = Vec::new();

    // package.json — JSON parse `dependencies` + `devDependencies`.
    if let Some(text) = read_manifest(project_root, "package.json") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            let mut seen_keys: Vec<String> = Vec::new();
            for field in ["dependencies", "devDependencies"] {
                if let Some(obj) = v.get(field).and_then(serde_json::Value::as_object) {
                    for k in obj.keys() {
                        seen_keys.push(k.clone());
                    }
                }
            }
            for key in &seen_keys {
                if let Some(name) = match key.as_str() {
                    "next" => Some("Next.js"),
                    "react" => Some("React"),
                    "vue" => Some("Vue"),
                    "svelte" => Some("Svelte"),
                    "@angular/core" => Some("Angular"),
                    "vite" => Some("Vite"),
                    _ => None,
                } {
                    push_unique(&mut found, name);
                }
            }
        }
    }

    // Cargo.toml — simple text scan against `[dependencies]` table keys.
    if let Some(text) = read_manifest(project_root, "Cargo.toml") {
        if cargo_has_dep(&text, "axum") {
            push_unique(&mut found, "Axum");
        }
        if cargo_has_dep(&text, "actix-web") {
            push_unique(&mut found, "Actix-Web");
        }
        if cargo_has_dep(&text, "rocket") {
            push_unique(&mut found, "Rocket");
        }
    }

    // pubspec.yaml — presence alone signals Flutter / Dart.
    if read_manifest(project_root, "pubspec.yaml").is_some() {
        push_unique(&mut found, "Flutter");
    }

    // Gemfile — substring search for `rails`.
    if let Some(text) = read_manifest(project_root, "Gemfile") {
        if gem_has(&text, "rails") {
            push_unique(&mut found, "Rails");
        }
    }

    // pyproject.toml + requirements.txt — substring search per framework.
    let py_text = read_manifest(project_root, "pyproject.toml")
        .or_else(|| read_manifest(project_root, "requirements.txt"))
        .unwrap_or_default();
    if !py_text.is_empty() {
        if python_has(&py_text, "django") {
            push_unique(&mut found, "Django");
        }
        if python_has(&py_text, "fastapi") {
            push_unique(&mut found, "FastAPI");
        }
        if python_has(&py_text, "flask") {
            push_unique(&mut found, "Flask");
        }
    }

    // pom.xml or build.gradle — Spring Boot.
    let java_text = read_manifest(project_root, "pom.xml")
        .or_else(|| read_manifest(project_root, "build.gradle"))
        .or_else(|| read_manifest(project_root, "build.gradle.kts"))
        .unwrap_or_default();
    if !java_text.is_empty() && java_text.contains("spring-boot") {
        push_unique(&mut found, "Spring Boot");
    }

    // composer.json — Laravel via `laravel/framework`.
    if let Some(text) = read_manifest(project_root, "composer.json") {
        if text.contains("laravel/framework") {
            push_unique(&mut found, "Laravel");
        }
    }

    let mut out: Vec<String> = found.iter().map(|s| s.to_string()).collect();
    out.sort();
    out.dedup();
    out
}

/// Read a manifest file from `project_root` if it exists.
fn read_manifest(project_root: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(project_root.join(name)).ok()
}

/// Push `s` into `out` only if not already present. O(n) but n is tiny.
fn push_unique(out: &mut Vec<&'static str>, s: &'static str) {
    if !out.contains(&s) {
        out.push(s);
    }
}

/// True if `text` declares `name` under a Cargo `[dependencies]`-style table.
///
/// Why: avoids pulling in a full TOML parser just to look for crate names.
/// Cargo manifests put each dep on its own line as `name = "..."` or
/// `name = { ... }`, so a line-prefix match is sufficient.
/// What: scans lines and returns true if any line (after trimming) starts
/// with `name` followed by whitespace or `=`.
fn cargo_has_dep(text: &str, name: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(name) {
            // Next char must be whitespace or '=' to avoid `name-suffix` matches.
            if rest.starts_with(|c: char| c.is_whitespace() || c == '=') {
                return true;
            }
        }
    }
    false
}

/// True if `text` (a Gemfile) declares the gem `name`.
fn gem_has(text: &str, name: &str) -> bool {
    let needle_single = format!("gem '{name}'");
    let needle_double = format!("gem \"{name}\"");
    text.contains(&needle_single) || text.contains(&needle_double)
}

/// True if `text` (pyproject.toml or requirements.txt) references `name`.
///
/// Why: pyproject.toml lists deps in `[project.dependencies]` (PEP 621),
/// `[tool.poetry.dependencies]`, or `[tool.uv.sources]`; requirements.txt
/// lists them one per line. A case-insensitive substring match covers all
/// formats without a full TOML parser.
/// What: lowercases both haystack and needle, then substring-matches.
fn python_has(text: &str, name: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&name.to_ascii_lowercase())
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
        assert_eq!(LanguageDetector::detect_file("README.md"), None);
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

    // --- Extension coverage via detect_file ----------------------------------

    #[test]
    fn detect_file_rust_case_insensitive() {
        assert_eq!(LanguageDetector::detect_file("foo.rs"), Some("rust".into()));
        assert_eq!(LanguageDetector::detect_file("foo.RS"), Some("rust".into()));
        assert_eq!(LanguageDetector::detect_file("foo.rust"), None);
    }

    #[test]
    fn detect_file_covers_all_js_variants() {
        for ext in [".js", ".jsx", ".mjs", ".cjs"] {
            let path = format!("a{ext}");
            assert_eq!(
                LanguageDetector::detect_file(&path),
                Some("javascript".into()),
                "ext={ext}"
            );
        }
        assert_eq!(
            LanguageDetector::detect_file("a.ts"),
            Some("typescript".into())
        );
    }

    #[test]
    fn detect_file_covers_cpp_and_c_extensions() {
        for ext in [".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".c", ".h"] {
            let path = format!("file{ext}");
            assert_eq!(
                LanguageDetector::detect_file(&path),
                Some("cpp".into()),
                "ext={ext}"
            );
        }
        assert_eq!(LanguageDetector::detect_file("file.txt"), None);
    }

    #[test]
    fn matches_basename_handles_root_and_nested() {
        assert!(matches_basename("cargo.toml", &["cargo.toml"]));
        assert!(matches_basename("a/b/cargo.toml", &["cargo.toml"]));
        assert!(!matches_basename("cargo.toml.bak", &["cargo.toml"]));
        assert!(!matches_basename("notcargo.toml", &["cargo.toml"]));
    }

    // --- detect_frameworks tests --------------------------------------------

    fn write_manifest(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).expect("write manifest");
    }

    #[test]
    fn detect_frameworks_finds_nextjs_and_react_in_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "package.json",
            r#"{ "dependencies": { "next": "14", "react": "18" } }"#,
        );
        let fws = detect_frameworks(tmp.path());
        assert!(fws.contains(&"Next.js".to_string()), "got {fws:?}");
        assert!(fws.contains(&"React".to_string()), "got {fws:?}");
    }

    #[test]
    fn detect_frameworks_finds_vite_in_dev_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "package.json",
            r#"{ "devDependencies": { "vite": "^5" } }"#,
        );
        let fws = detect_frameworks(tmp.path());
        assert_eq!(fws, vec!["Vite".to_string()]);
    }

    #[test]
    fn detect_frameworks_finds_axum_in_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "Cargo.toml",
            "[dependencies]\naxum = \"0.7\"\nserde = \"1\"\n",
        );
        let fws = detect_frameworks(tmp.path());
        assert!(fws.contains(&"Axum".to_string()), "got {fws:?}");
    }

    #[test]
    fn detect_frameworks_finds_django_and_fastapi_in_requirements() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "requirements.txt",
            "Django==4.2\nfastapi==0.110\nrequests==2.31\n",
        );
        let fws = detect_frameworks(tmp.path());
        assert!(fws.contains(&"Django".to_string()), "got {fws:?}");
        assert!(fws.contains(&"FastAPI".to_string()), "got {fws:?}");
    }

    #[test]
    fn detect_frameworks_finds_rails_in_gemfile() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "Gemfile",
            "source 'https://rubygems.org'\ngem 'rails', '~> 7.0'\n",
        );
        let fws = detect_frameworks(tmp.path());
        assert_eq!(fws, vec!["Rails".to_string()]);
    }

    #[test]
    fn detect_frameworks_finds_flutter_via_pubspec_presence() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(tmp.path(), "pubspec.yaml", "name: my_app\n");
        let fws = detect_frameworks(tmp.path());
        assert_eq!(fws, vec!["Flutter".to_string()]);
    }

    #[test]
    fn detect_frameworks_finds_spring_boot_in_pom() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "pom.xml",
            "<project><dependencies><dependency><groupId>org.springframework.boot</groupId>\
             <artifactId>spring-boot-starter-web</artifactId></dependency></dependencies></project>",
        );
        let fws = detect_frameworks(tmp.path());
        assert_eq!(fws, vec!["Spring Boot".to_string()]);
    }

    #[test]
    fn detect_frameworks_finds_laravel_in_composer_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "composer.json",
            r#"{ "require": { "laravel/framework": "^10.0" } }"#,
        );
        let fws = detect_frameworks(tmp.path());
        assert_eq!(fws, vec!["Laravel".to_string()]);
    }

    #[test]
    fn detect_frameworks_returns_sorted_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        // React listed in both sections — should appear only once.
        write_manifest(
            tmp.path(),
            "package.json",
            r#"{
                "dependencies": { "react": "18", "next": "14" },
                "devDependencies": { "react": "18", "vite": "5" }
            }"#,
        );
        let fws = detect_frameworks(tmp.path());
        let mut expected = vec![
            "Next.js".to_string(),
            "React".to_string(),
            "Vite".to_string(),
        ];
        expected.sort();
        assert_eq!(fws, expected);
    }

    #[test]
    fn detect_frameworks_returns_empty_for_empty_project() {
        let tmp = tempfile::tempdir().unwrap();
        let fws = detect_frameworks(tmp.path());
        assert!(fws.is_empty(), "got {fws:?}");
    }

    #[test]
    fn cargo_has_dep_avoids_prefix_collisions() {
        let toml = "[dependencies]\naxum-extra = \"0.9\"\n";
        // `axum` must NOT match `axum-extra` since the next char is '-' not '=' or whitespace.
        assert!(!cargo_has_dep(toml, "axum"));
        let toml2 = "[dependencies]\naxum = \"0.7\"\n";
        assert!(cargo_has_dep(toml2, "axum"));
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
