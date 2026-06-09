//! HTTP route handler submodules.
//!
//! Why: Groups handlers by feature area so each file stays well under the
//! 500-line cap and readers can find handlers by domain without scanning the
//! entire service module.
//!
//! What: Re-exports the five handler modules:
//! - `analysis` — complexity hotspots, smells, quality, refactor, diagnostics
//! - `graph` — KG graph/entities, clustering, NER, SCIP ingest
//! - `facts` — CRUD for the FactStore knowledge triples
//! - `review` — diff review, GitHub PR review, webhooks
//! - `deep` — LLM deep-analysis pass (`POST /analyze/deep`)
//!
//! Test: All handler tests live in `service/tests.rs` and `service/tests_review.rs`.

pub mod analysis;
pub mod deep;
pub mod facts;
pub mod graph;
pub mod review;

/// Map a file path to a language tag by its extension.
///
/// Why: Both `handlers/analysis.rs` (refactor suggestions) and
/// `handlers/deep.rs` (synthesis) need to detect the language for a chunk
/// by file extension. Centralising the mapping here eliminates the
/// duplication and ensures both call sites stay in sync when new languages
/// are added.
/// What: Lowercases the path, matches on the final extension segment, and
/// returns a `&'static str` language tag. Returns `"unknown"` for extensions
/// that are not explicitly mapped.
/// Test: `lang_for_extension_*` unit tests below cover every mapped extension
/// plus the unknown fallback.
pub(crate) fn lang_for_extension(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    // Walk by suffix: `.d.ts` files end with `.ts` and are intentionally matched
    // by the `.ts` arm below (returns "typescript"), which is correct — `.d.ts`
    // files are TypeScript declaration files and should be analysed as TypeScript.
    if lower.ends_with(".rs") {
        "rust"
    } else if lower.ends_with(".tsx") {
        "tsx"
    } else if lower.ends_with(".ts") {
        "typescript"
    } else if lower.ends_with(".jsx") {
        "jsx"
    } else if lower.ends_with(".js") {
        "javascript"
    } else if lower.ends_with(".py") {
        "python"
    } else if lower.ends_with(".go") {
        "go"
    } else if lower.ends_with(".java") {
        "java"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rust extension maps to "rust".
    #[test]
    fn lang_for_extension_rust() {
        assert_eq!(lang_for_extension("src/main.rs"), "rust");
    }

    /// TypeScript extension maps to "typescript".
    #[test]
    fn lang_for_extension_ts() {
        assert_eq!(lang_for_extension("src/app.ts"), "typescript");
    }

    /// TSX extension maps to "tsx".
    #[test]
    fn lang_for_extension_tsx() {
        assert_eq!(lang_for_extension("src/App.tsx"), "tsx");
    }

    /// JSX extension maps to "jsx".
    #[test]
    fn lang_for_extension_jsx() {
        assert_eq!(lang_for_extension("src/App.jsx"), "jsx");
    }

    /// JavaScript extension maps to "javascript".
    #[test]
    fn lang_for_extension_js() {
        assert_eq!(lang_for_extension("index.js"), "javascript");
    }

    /// Python extension maps to "python".
    #[test]
    fn lang_for_extension_py() {
        assert_eq!(lang_for_extension("scripts/run.py"), "python");
    }

    /// Go extension maps to "go".
    #[test]
    fn lang_for_extension_go() {
        assert_eq!(lang_for_extension("main.go"), "go");
    }

    /// Java extension maps to "java".
    #[test]
    fn lang_for_extension_java() {
        assert_eq!(lang_for_extension("Main.java"), "java");
    }

    /// Unknown extension returns "unknown".
    #[test]
    fn lang_for_extension_unknown() {
        assert_eq!(lang_for_extension("Makefile"), "unknown");
        assert_eq!(lang_for_extension("data.csv"), "unknown");
    }

    /// Extension matching is case-insensitive.
    #[test]
    fn lang_for_extension_case_insensitive() {
        assert_eq!(lang_for_extension("main.RS"), "rust");
        assert_eq!(lang_for_extension("App.TSX"), "tsx");
    }

    /// `.d.ts` declaration files are matched by the `.ts` arm (returns "typescript").
    ///
    /// Why: TypeScript declaration files share the `.ts` suffix; they should be
    /// analysed as TypeScript, not treated as unknown.
    #[test]
    fn lang_for_extension_dts_is_typescript() {
        assert_eq!(lang_for_extension("src/index.d.ts"), "typescript");
        assert_eq!(lang_for_extension("types/global.D.TS"), "typescript");
    }
}
