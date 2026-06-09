//! Language-specific static analysis adapters for trusty-analyzer.
//!
//! Why: Each supported language needs its own tree-sitter grammar walk to
//! extract symbols (functions, classes, imports) into a language-neutral
//! `KgGraph`. This crate isolates per-language code from the analyzer service
//! and from the workspace-wide types in `trusty-common`.
//!
//! What: Defines the `LanguageAnalyzer` trait, file-extension-based language
//! detection, and a set of adapters: Rust and TypeScript/JavaScript are
//! fully implemented; Python, Java, and Go are stubbed for Phase 2b.
//!
//! Test: each adapter has a unit test that parses a minimal source snippet
//! and asserts at least one expected node is extracted.

pub mod adapters;
pub mod detection;
pub mod ext_map;
#[allow(clippy::module_inception)]
pub mod lang;

// Re-export tree-sitter so downstream crates can build queries
// without needing their own dependency on it.
pub use tree_sitter;

pub use adapters::c::CAnalyzer;
pub use adapters::cpp::CppAnalyzer;
pub use adapters::csharp::CSharpAnalyzer;
pub use adapters::javascript::JavaScriptAnalyzer;
pub use adapters::kotlin::KotlinAnalyzer;
pub use adapters::php::PhpAnalyzer;
pub use adapters::ruby::RubyAnalyzer;
pub use adapters::rust::RustAnalyzer;
pub use adapters::scala::ScalaAnalyzer;
pub use adapters::swift::SwiftAnalyzer;
pub use adapters::typescript::TypeScriptAnalyzer;
pub use adapters::{go::GoAnalyzer, java::JavaAnalyzer, python::PythonAnalyzer};
pub use detection::{detect_frameworks, DetectionResult, LanguageDetector};
pub use ext_map::{lang_for_extension, lang_for_linter};
pub use lang::{LanguageAnalyzer, StaticAnalysisResult};
