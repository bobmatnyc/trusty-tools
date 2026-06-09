//! Canonical extension-to-language-tag table.
//!
//! Why: trusty-analyze previously had four independent extension→language maps
//! (the router `LANGUAGE_DETECTORS`, adapter `supported_extensions()`, the
//! service `lang_for_extension`, and `core::review::language_for_path`). They
//! drifted apart, causing silent zero-result bugs (#963, #968, #971).
//!
//! What: One `EXT_MAP` slice covering every recognized source extension with
//! two tags per entry — the *display tag* (tsx/jsx kept distinct, used for
//! LLM hints and syntax highlighting) and the *linter tag* (tsx→typescript,
//! jsx→javascript, used for tool-registry lookup). All other consumers derive
//! from these two helpers:
//! - `lang_for_extension(path)` — display tag (`"tsx"`, `"jsx"`, …)
//! - `lang_for_linter(path)` — linter/router tag (`"typescript"`, `"javascript"`, …)
//!
//! Test: `ext_map_completeness` asserts every adapter's `supported_extensions()`
//! is a strict subset of `EXT_MAP`; `linter_tag_routes_registered_tools`
//! asserts every known `StaticTool::language()` tag can be produced by
//! `lang_for_linter` for at least one extension in the map.

/// One row in the canonical extension table.
///
/// - `ext`: file extension including the leading dot, lowercase.
/// - `display`: language tag returned to LLM callers / syntax highlighters.
/// - `linter`: language tag used to look up `StaticTool` entries in
///   `ToolRegistry`. Usually equals `display`; differs only for `tsx`
///   (→ `"typescript"`) and `jsx` (→ `"javascript"`).
pub struct ExtEntry {
    pub ext: &'static str,
    pub display: &'static str,
    pub linter: &'static str,
}

/// Macro to build a same-display-and-linter entry.
macro_rules! e {
    ($ext:literal, $lang:literal) => {
        ExtEntry {
            ext: $ext,
            display: $lang,
            linter: $lang,
        }
    };
    ($ext:literal, $display:literal, $linter:literal) => {
        ExtEntry {
            ext: $ext,
            display: $display,
            linter: $linter,
        }
    };
}

/// Canonical extension→language table.
///
/// Entries are ordered: more-specific extensions first (`.d.ts` before `.ts`,
/// `.tsx` before `.ts`), so the first matching entry wins when callers iterate.
/// Extensions within a language group are ordered specific→general.
///
/// Why: a single slice here is the only place a new extension needs to be
/// added; every other consumer calls `lang_for_extension` or
/// `lang_for_linter` and automatically picks up the change.
/// What: a `&'static [ExtEntry]` with one row per recognized extension.
/// Test: `ext_map_no_duplicate_entries` (below) asserts every ext appears at
/// most once; `ext_map_completeness` (in `detection.rs`) checks adapters.
pub static EXT_MAP: &[ExtEntry] = &[
    // ── Rust ────────────────────────────────────────────────────────────────
    e!(".rs", "rust"),
    // ── TypeScript / TSX / module variants ──────────────────────────────────
    // Order: .tsx before .ts so the suffix check short-circuits on tsx paths.
    e!(".tsx", "tsx", "typescript"),
    // MUST precede `.ts` — `ends_with` suffix match; see ext_map_suffix_ordering_invariant test.
    e!(".mts", "typescript"),
    // MUST precede `.ts` — `ends_with` suffix match; see ext_map_suffix_ordering_invariant test.
    e!(".cts", "typescript"),
    e!(".ts", "typescript"),
    // ── JavaScript / JSX / module variants ──────────────────────────────────
    e!(".jsx", "jsx", "javascript"),
    e!(".mjs", "javascript"),
    e!(".cjs", "javascript"),
    e!(".js", "javascript"),
    // ── Python ──────────────────────────────────────────────────────────────
    e!(".pyw", "python"),
    e!(".pyi", "python"),
    e!(".py", "python"),
    // ── Java ────────────────────────────────────────────────────────────────
    e!(".java", "java"),
    // ── Go ──────────────────────────────────────────────────────────────────
    e!(".go", "go"),
    // ── Kotlin ──────────────────────────────────────────────────────────────
    // MUST precede `.ts` — `ends_with` suffix match; see ext_map_suffix_ordering_invariant test.
    e!(".kts", "kotlin"),
    e!(".kt", "kotlin"),
    // ── Swift ───────────────────────────────────────────────────────────────
    e!(".swift", "swift"),
    // ── Ruby ────────────────────────────────────────────────────────────────
    e!(".gemspec", "ruby"),
    e!(".rake", "ruby"),
    e!(".ru", "ruby"),
    e!(".rb", "ruby"),
    // ── PHP ─────────────────────────────────────────────────────────────────
    e!(".phtml", "php"),
    e!(".php", "php"),
    // ── C++ ─────────────────────────────────────────────────────────────────
    // Note: `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx` route to `"cpp"`.
    // `.c` and `.h` route to the `"c"` adapter (tree-sitter-c grammar), but
    // clang-tidy registers under `"cpp"` so the linter tag is also `"cpp"`.
    e!(".cpp", "cpp"),
    e!(".cc", "cpp"),
    e!(".cxx", "cpp"),
    e!(".hpp", "cpp"),
    e!(".hh", "cpp"),
    e!(".hxx", "cpp"),
    // ── C ───────────────────────────────────────────────────────────────────
    e!(".c", "c", "cpp"),
    e!(".h", "c", "cpp"),
    // ── C# ──────────────────────────────────────────────────────────────────
    e!(".cs", "csharp"),
    // ── Scala ───────────────────────────────────────────────────────────────
    e!(".scala", "scala"),
];

/// Map a file path to its **display** language tag by extension.
///
/// Why: LLM callers, syntax highlighters, and refactor suggestions need the
/// finest-grained tag (`"tsx"`, `"jsx"`) so prompts can mention the correct
/// dialect. Centralising the mapping here eliminates duplication between
/// `service/handlers/mod.rs` and `core/review.rs` (both previously contained
/// identical `if/else if` chains that diverged over time).
///
/// What: Lowercases the path, iterates `EXT_MAP` from the top, returns the
/// first `entry.display` where `lower_path.ends_with(entry.ext)`. Returns
/// `"unknown"` for unrecognized extensions.
///
/// Test: `lang_for_extension_*` unit tests in this file cover every mapped
/// extension plus the unknown fallback and case-insensitivity.
pub fn lang_for_extension(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    EXT_MAP
        .iter()
        .find(|e| lower.ends_with(e.ext))
        .map(|e| e.display)
        .unwrap_or("unknown")
}

/// Map a file path to its **linter** language tag by extension.
///
/// Why: `ToolRegistry` is keyed by the language tag returned by each
/// `StaticTool::language()`. For TSX/JSX files the linter tag differs from
/// the display tag: `.tsx` → `"typescript"` (BiomeTool registers there),
/// `.jsx` → `"javascript"`. For all other languages the two tags are equal.
///
/// What: Lowercases the path, iterates `EXT_MAP`, returns the first
/// `entry.linter` where `lower_path.ends_with(entry.ext)`. Returns `None`
/// for unrecognized extensions (so callers can skip linting unknown files).
///
/// Test: `lang_for_linter_tsx_routes_to_typescript` and
/// `lang_for_linter_c_routes_to_cpp` below.
pub fn lang_for_linter(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    EXT_MAP
        .iter()
        .find(|e| lower.ends_with(e.ext))
        .map(|e| e.linter)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── lang_for_extension tests ─────────────────────────────────────────────

    #[test]
    fn lang_for_extension_rust() {
        assert_eq!(lang_for_extension("src/main.rs"), "rust");
        assert_eq!(lang_for_extension("LIB.RS"), "rust"); // case-insensitive
    }

    #[test]
    fn lang_for_extension_typescript_variants() {
        assert_eq!(lang_for_extension("app.ts"), "typescript");
        assert_eq!(lang_for_extension("src/index.d.ts"), "typescript");
        assert_eq!(lang_for_extension("mod.mts"), "typescript");
        assert_eq!(lang_for_extension("mod.cts"), "typescript");
    }

    #[test]
    fn lang_for_extension_tsx_is_distinct() {
        assert_eq!(lang_for_extension("App.tsx"), "tsx");
        assert_eq!(lang_for_extension("App.TSX"), "tsx");
    }

    #[test]
    fn lang_for_extension_javascript_variants() {
        assert_eq!(lang_for_extension("index.js"), "javascript");
        assert_eq!(lang_for_extension("mod.mjs"), "javascript");
        assert_eq!(lang_for_extension("mod.cjs"), "javascript");
    }

    #[test]
    fn lang_for_extension_jsx_is_distinct() {
        assert_eq!(lang_for_extension("App.jsx"), "jsx");
    }

    #[test]
    fn lang_for_extension_python_variants() {
        assert_eq!(lang_for_extension("script.py"), "python");
        assert_eq!(lang_for_extension("stubs.pyi"), "python");
        assert_eq!(lang_for_extension("gui.pyw"), "python");
    }

    #[test]
    fn lang_for_extension_jvm_languages() {
        assert_eq!(lang_for_extension("Main.java"), "java");
        assert_eq!(lang_for_extension("Main.kt"), "kotlin");
        assert_eq!(lang_for_extension("build.kts"), "kotlin");
        assert_eq!(lang_for_extension("Main.scala"), "scala");
    }

    #[test]
    fn lang_for_extension_systems_languages() {
        assert_eq!(lang_for_extension("main.go"), "go");
        assert_eq!(lang_for_extension("foo.swift"), "swift");
        assert_eq!(lang_for_extension("lib.cpp"), "cpp");
        assert_eq!(lang_for_extension("lib.cc"), "cpp");
        assert_eq!(lang_for_extension("lib.cxx"), "cpp");
        assert_eq!(lang_for_extension("include.hpp"), "cpp");
        assert_eq!(lang_for_extension("include.hh"), "cpp");
        assert_eq!(lang_for_extension("include.hxx"), "cpp");
        assert_eq!(lang_for_extension("main.c"), "c");
        assert_eq!(lang_for_extension("header.h"), "c");
        assert_eq!(lang_for_extension("app.cs"), "csharp");
    }

    #[test]
    fn lang_for_extension_ruby_variants() {
        assert_eq!(lang_for_extension("app.rb"), "ruby");
        assert_eq!(lang_for_extension("task.rake"), "ruby");
        assert_eq!(lang_for_extension("gem.gemspec"), "ruby");
        assert_eq!(lang_for_extension("config.ru"), "ruby");
    }

    #[test]
    fn lang_for_extension_php_variants() {
        assert_eq!(lang_for_extension("index.php"), "php");
        assert_eq!(lang_for_extension("template.phtml"), "php");
    }

    #[test]
    fn lang_for_extension_unknown_fallback() {
        assert_eq!(lang_for_extension("Makefile"), "unknown");
        assert_eq!(lang_for_extension("data.csv"), "unknown");
        assert_eq!(lang_for_extension("README.md"), "unknown");
    }

    // ── lang_for_linter tests ────────────────────────────────────────────────

    #[test]
    fn lang_for_linter_tsx_routes_to_typescript() {
        assert_eq!(lang_for_linter("App.tsx"), Some("typescript"));
    }

    #[test]
    fn lang_for_linter_jsx_routes_to_javascript() {
        assert_eq!(lang_for_linter("App.jsx"), Some("javascript"));
    }

    #[test]
    fn lang_for_linter_c_routes_to_cpp() {
        // clang-tidy registers under "cpp"; .c/.h must route there too.
        assert_eq!(lang_for_linter("main.c"), Some("cpp"));
        assert_eq!(lang_for_linter("header.h"), Some("cpp"));
    }

    #[test]
    fn lang_for_linter_unknown_is_none() {
        assert_eq!(lang_for_linter("Makefile"), None);
        assert_eq!(lang_for_linter("data.json"), None);
    }

    // ── table integrity ──────────────────────────────────────────────────────

    #[test]
    fn ext_map_no_duplicate_entries() {
        let mut seen = std::collections::HashSet::new();
        for entry in EXT_MAP {
            assert!(
                seen.insert(entry.ext),
                "duplicate extension in EXT_MAP: {:?}",
                entry.ext
            );
        }
    }

    /// EXT_MAP must list longer/more-specific suffixes before the shorter ones they subsume.
    ///
    /// Why: `lang_for_extension` and `lang_for_linter` both use `ends_with` and
    /// return the *first* match. If a shorter suffix (e.g. `.ts`) appears before a
    /// longer one that ends with it (e.g. `.mts`, `.cts`, `.kts`), every file
    /// matching the longer suffix is silently misrouted to the shorter suffix's
    /// language — `.kts` would return `"typescript"` instead of `"kotlin"` with no
    /// compile-time or startup warning.
    ///
    /// What: iterates every ordered pair `(a_idx, b_idx)` of distinct EXT_MAP entries;
    /// when `EXT_MAP[a_idx].ext` ends with `EXT_MAP[b_idx].ext` and is strictly
    /// longer, asserts `a_idx < b_idx` (the more-specific suffix precedes the
    /// less-specific one).
    ///
    /// Test: this function IS the test.
    #[test]
    fn ext_map_suffix_ordering_invariant() {
        for (a_idx, a_entry) in EXT_MAP.iter().enumerate() {
            for (b_idx, b_entry) in EXT_MAP.iter().enumerate() {
                if a_idx == b_idx {
                    continue;
                }
                // a is longer and its tail matches b → a is more-specific, must come first.
                if a_entry.ext.ends_with(b_entry.ext) && a_entry.ext.len() > b_entry.ext.len() {
                    assert!(
                        a_idx < b_idx,
                        "EXT_MAP suffix-ordering violation: {:?} (index {}) ends with {:?} \
                         (index {}) but appears AFTER it — the more-specific suffix must \
                         precede the less-specific one so `ends_with` matching routes \
                         correctly (e.g. `.kts` must precede `.ts`)",
                        a_entry.ext,
                        a_idx,
                        b_entry.ext,
                        b_idx,
                    );
                }
            }
        }
    }

    /// Every adapter's `supported_extensions()` must be a subset of EXT_MAP.
    ///
    /// Why: if an adapter claims to support an extension that has no EXT_MAP
    /// entry, files with that extension will silently fall through the
    /// `lang_for_extension` / `lang_for_linter` helpers returning "unknown",
    /// which breaks diagnostics routing and LLM hints. This test is the
    /// mechanical guard that prevents future adapters from drifting.
    ///
    /// What: constructs every `LanguageAnalyzer` adapter, collects all their
    /// `supported_extensions()` slices, then asserts each extension is present
    /// in EXT_MAP.
    ///
    /// Test: this function IS the test.
    #[test]
    fn ext_map_covers_all_adapter_extensions() {
        use crate::lang::{
            CAnalyzer, CSharpAnalyzer, CppAnalyzer, GoAnalyzer, JavaAnalyzer, JavaScriptAnalyzer,
            KotlinAnalyzer, LanguageAnalyzer, PhpAnalyzer, PythonAnalyzer, RubyAnalyzer,
            RustAnalyzer, ScalaAnalyzer, SwiftAnalyzer, TypeScriptAnalyzer,
        };

        let ext_set: std::collections::HashSet<&str> = EXT_MAP.iter().map(|e| e.ext).collect();

        let adapters: Vec<Box<dyn LanguageAnalyzer>> = vec![
            Box::new(RustAnalyzer::new()),
            Box::new(TypeScriptAnalyzer::new()),
            Box::new(JavaScriptAnalyzer::new()),
            Box::new(PythonAnalyzer::new()),
            Box::new(JavaAnalyzer::new()),
            Box::new(GoAnalyzer::new()),
            Box::new(CppAnalyzer::new()),
            Box::new(CAnalyzer::new()),
            Box::new(KotlinAnalyzer::new()),
            Box::new(SwiftAnalyzer::new()),
            Box::new(RubyAnalyzer::new()),
            Box::new(PhpAnalyzer::new()),
            Box::new(CSharpAnalyzer::new()),
            Box::new(ScalaAnalyzer::new()),
        ];

        for adapter in &adapters {
            for ext in adapter.supported_extensions() {
                assert!(
                    ext_set.contains(ext),
                    "adapter '{}' has extension {:?} not present in EXT_MAP — add it",
                    adapter.language(),
                    ext,
                );
            }
        }
    }
}
