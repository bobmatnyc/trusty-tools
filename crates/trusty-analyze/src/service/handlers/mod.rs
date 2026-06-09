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
/// by file extension. This function delegates to the single canonical
/// extension table in `lang::ext_map` so all service-layer consumers
/// stay in sync when new languages or extensions are added.
/// What: Delegates to `crate::lang::ext_map::lang_for_extension`, which
/// returns the finest-grained display tag (`"tsx"`, `"jsx"`, etc.) or
/// `"unknown"` for unrecognized extensions. Case-insensitive.
/// Test: `lang_for_extension_*` unit tests below cover the key extension
/// groups via the canonical table; the full coverage lives in
/// `crate::lang::ext_map::tests`.
pub(crate) fn lang_for_extension(path: &str) -> &'static str {
    crate::lang::ext_map::lang_for_extension(path)
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
