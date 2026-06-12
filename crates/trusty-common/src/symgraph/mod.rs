//! Standalone symbol-graph engine (formerly the `trusty-symgraph` crate).
//!
//! Why: Agents should operate in symbol space, not file space. Files are a
//! derived artifact of the registry. Bundling parse → registry → emit and
//! the editor primitives behind one library module lets other tools consume
//! the substrate without depending on the orchestrator binary. The crate
//! was absorbed into `trusty-common` (issue #5 phase 2c) so the entire
//! trusty-* toolchain links a single internal library.
//! What: Re-exports the same surface the legacy `trusty-symgraph` crate
//! exposed — `Symbol`/`SymbolKind`/`SymbolRegistry`/`SymbolGraph`/`Patch`
//! / edit helpers — backed by tree-sitter parsing, an `IndexMap`-based
//! content-addressed registry, and a deterministic emitter. The `symgraph`
//! feature exposes the pure-data contracts surface (no tree-sitter, no
//! `links` conflict); the `symgraph-parser` feature pulls in tree-sitter
//! and the full emitter stack; the `symgraph-server` feature additionally
//! exposes the HTTP server.
//! Test: Each submodule keeps its existing unit tests; integration tests
//! in `crates/trusty-symgraph/tests/` cover the round-trip through the
//! thin re-export shim.

// Always-on contracts surface — pure data types, no tree-sitter.
// Downstream crates that only need EntityType / RawEntity / EdgeKind /
// fact_hash_str / tables can depend on this crate with the `symgraph`
// feature and avoid the `links = "tree-sitter"` conflict.
pub mod contracts;

// Parser-path modules — gated behind `symgraph-parser` feature so
// non-tree-sitter consumers can import the contracts surface without
// pulling in grammars.
#[cfg(feature = "symgraph-parser")]
pub mod editor;
#[cfg(feature = "symgraph-parser")]
pub mod emitter;
#[cfg(feature = "symgraph-parser")]
pub mod graph;
#[cfg(feature = "symgraph-parser")]
pub mod parser;
#[cfg(feature = "symgraph-parser")]
pub mod registry;
#[cfg(feature = "symgraph-parser")]
pub mod symbol;

// INTENT: Declare the strategy trait and default module-path implementation.
#[cfg(feature = "symgraph-parser")]
pub mod strategy;

// INTENT: Declare the SCC-based locality clustering strategy.
#[cfg(feature = "symgraph-parser")]
pub mod locality;

// INTENT: Declare the test-colocation strategy that places tests next to targets.
#[cfg(feature = "symgraph-parser")]
pub mod test_colocation;

#[cfg(feature = "symgraph-server")]
pub mod server;

// Public re-exports — mirrors the legacy `trusty_symgraph` crate root so
// downstream consumers can adopt the module by changing one path.
//
// Note: `contracts::EdgeKind` IS now re-exported through `graph::EdgeKind`
// (issue #815 convergence). `graph::EdgeKind` is a type alias to
// `contracts::EdgeKind`, so both paths refer to the same type.
// The canonical path is `trusty_common::symgraph::contracts::EdgeKind`;
// access via `trusty_common::symgraph::EdgeKind` also works
// (re-exported below via `graph::EdgeKind`).
pub use contracts::{EdgeKindError, EntityType, RawEntity, fact_hash_str};

#[cfg(feature = "symgraph-parser")]
pub use registry::{SymbolEntry, SymbolId, SymbolRegistry};

#[cfg(feature = "symgraph-parser")]
pub use editor::{
    Patch, add_import, apply_patch, emit_diff, insert_after_symbol, replace_symbol, validate_syntax,
};
#[cfg(feature = "symgraph-parser")]
pub use emitter::{LayoutRules, apply_emit, assign_file, emit};
#[cfg(feature = "symgraph-parser")]
pub use graph::{Edge, EdgeKind, SymbolEdge, SymbolGraph, SymbolNode};
#[cfg(feature = "symgraph-parser")]
pub use parser::{Language, file_to_module_path, parse_directory, parse_file};
#[cfg(feature = "symgraph-parser")]
pub use symbol::{Symbol, SymbolKind, detect_language, extract_symbols, get_symbol, list_symbols};

// Re-exports: strategies
#[cfg(feature = "symgraph-parser")]
pub use locality::LocalityStrategy;
#[cfg(feature = "symgraph-parser")]
pub use strategy::{EmitStrategy, ModulePathStrategy};
#[cfg(feature = "symgraph-parser")]
pub use test_colocation::TestColocationStrategy;

// Note: `to_petgraph` was removed in #356. `SymbolGraph` now stores a
// `petgraph::StableGraph` internally — call `SymbolGraph::inner()` to
// borrow it directly for petgraph algorithms.
