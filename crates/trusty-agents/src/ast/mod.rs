//! AST-aware code substrate (#347, extracted to `symgraph` in #351).
//!
//! Why: The bulk of this module now lives in the standalone `symgraph`
//! crate (`crates/symgraph/`). Keeping a thin re-export shim here means
//! the rest of `trusty-agents` (and especially `tools::ast_tools` +
//! `agents::in_process_runner`) compiles unchanged after the extraction.
//! What: Re-exports the same surface previously emitted by this module
//! plus the local `AST_NATIVE_OVERRIDE` atomic — that flag is bound to
//! the orchestrator binary, not the substrate.
//! Test: `cargo test ast::` still works because the re-exports preserve
//! the legacy paths; the canonical tests now live in `crates/symgraph/`.

pub use trusty_common::symgraph::{editor, emitter, graph as kg, parser, registry, symbol};

pub use trusty_common::symgraph::parser::{Language, file_to_module_path};
pub use trusty_common::symgraph::{
    Edge, EdgeKind, LayoutRules, Patch, Symbol, SymbolEntry, SymbolGraph, SymbolId, SymbolKind,
    SymbolNode, SymbolRegistry, add_import, apply_emit, apply_patch, assign_file, detect_language,
    emit, emit_diff, extract_symbols, get_symbol, insert_after_symbol, list_symbols,
    parse_directory, parse_file, replace_symbol, validate_syntax,
};

// Re-export strategy types added in #362.
pub use trusty_common::symgraph::{
    EmitStrategy, LocalityStrategy, ModulePathStrategy, TestColocationStrategy,
};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// Process-global pre-indexed `SymbolRegistry` for the active workflow run.
///
/// Why: When workflow agents (research/plan/code) start against an existing
/// codebase, the AST-native tool surface (`get_symbol`, etc.) needs a
/// structural view of the project on entry — not an empty registry that
/// must be populated lazily one file at a time. Pre-indexing the project
/// directory once before the phase loop starts gives agents O(1) symbol
/// lookups across every supported file.
/// What: `OnceLock<Arc<RwLock<SymbolRegistry>>>` — set exactly once per
/// process by the workflow engine; readers (e.g. `GetSymbolTool`) take a
/// shared lock for lookups. `OnceLock` keeps the API safe in the face of
/// repeated calls (the second `set` is silently dropped).
/// Test: `pre_indexed_registry_round_trip` in `src/ast/mod.rs` asserts that
/// `set_pre_indexed_registry` followed by `get_pre_indexed_registry`
/// returns the same symbols.
static PRE_INDEXED_REGISTRY: OnceLock<Arc<RwLock<SymbolRegistry>>> = OnceLock::new();

/// Install the process-wide pre-indexed registry (called once from the
/// workflow engine before the phase loop starts).
///
/// Why: Centralising the install point avoids racy double-init from agents
/// that might also try to populate the cache.
/// What: Wraps `registry` in `Arc<RwLock<_>>` and calls `OnceLock::set`.
/// Subsequent calls are silently ignored (the first registry wins for the
/// process lifetime).
/// Test: `pre_indexed_registry_round_trip`.
pub fn set_pre_indexed_registry(registry: SymbolRegistry) {
    // Already initialised — replace the contents in place so subsequent
    // workflow runs in the same process see fresh symbols.
    if let Some(slot) = PRE_INDEXED_REGISTRY.get() {
        match slot.write() {
            Ok(mut guard) => {
                *guard = registry;
            }
            Err(e) => {
                tracing::warn!(error = %e, "pre-indexed registry lock poisoned; keeping previous");
            }
        }
        return;
    }
    // First install: stash the new Arc in the OnceLock.
    let _ = PRE_INDEXED_REGISTRY.set(Arc::new(RwLock::new(registry)));
}

/// Borrow the process-wide pre-indexed registry, if one has been installed.
///
/// Why: Tools that want to short-circuit on-demand parsing for already-known
/// files need a cheap, lock-shared view of the project's symbols.
/// What: Returns `Some(Arc<RwLock<…>>)` once `set_pre_indexed_registry` has
/// been called for this process; `None` otherwise.
/// Test: `pre_indexed_registry_round_trip`.
pub fn get_pre_indexed_registry() -> Option<Arc<RwLock<SymbolRegistry>>> {
    PRE_INDEXED_REGISTRY.get().cloned()
}

/// Bulk-parse a directory and tag every emitted entry with the file it came
/// from, so `GetSymbolTool` can filter the pre-indexed registry by file.
///
/// Why: `parser::parse_directory` leaves `SymbolEntry::assigned_file` as
/// `None` (it's intended for the emit path). The pre-index path needs a
/// reverse lookup — "which file produced this symbol?" — so we parse the
/// tree file-by-file and record each entry's source file.
/// What: Walks `dir`, parses every supported file via `parser::parse_file`,
/// sets `assigned_file = Some(path)` on each entry, and inserts into a fresh
/// registry rooted at `project_root`. Per-file parse failures are logged at
/// debug level and skipped (best-effort).
/// Test: `pre_index_directory_tags_assigned_file` in `src/ast/mod.rs`.
pub fn pre_index_directory(
    dir: &std::path::Path,
    project_root: &std::path::Path,
) -> anyhow::Result<SymbolRegistry> {
    use walkdir::WalkDir;
    let mut registry = SymbolRegistry::new(project_root.to_path_buf());
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || trusty_common::symgraph::parser::detect_language(path).is_none() {
            continue;
        }
        let path_str = path.to_string_lossy();
        if path_str.contains("/target/") || path_str.contains("/.git/") {
            continue;
        }
        match trusty_common::symgraph::parser::parse_file(path, project_root) {
            Ok(entries) => {
                for mut e in entries {
                    e.assigned_file = Some(path.to_path_buf());
                    registry.insert(e);
                }
            }
            Err(e) => {
                tracing::debug!("pre_index_directory skipped {}: {e}", path.display());
            }
        }
    }
    crate::events::emit(crate::events::Event::AstOperation {
        session_id: String::new(),
        op: "index".into(),
        detail: format!("{} symbols from {}", registry.len(), dir.display()),
    });
    Ok(registry)
}

/// Process-global override for the AST-native tool bundle (#348).
///
/// Why: The `--ast-native` CLI flag must override per-agent TOML so users
/// can opt in/out at invocation time without editing configs. The flag is
/// process-local, not part of the substrate, so it stays in the binary.
/// What: `AtomicBool`; set via `set_ast_native_override(true)` and consumed
/// by `is_ast_native_overridden()`.
/// Test: Implicit via the `--ast-native` CLI smoke test.
static AST_NATIVE_OVERRIDE: AtomicBool = AtomicBool::new(false);

/// Set the process-wide AST-native override (called at startup from `main`).
pub fn set_ast_native_override(on: bool) {
    AST_NATIVE_OVERRIDE.store(on, Ordering::SeqCst);
}

/// Read the process-wide AST-native override.
pub fn is_ast_native_overridden() -> bool {
    AST_NATIVE_OVERRIDE.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    #[serial_test::serial(pre_indexed_registry)]
    fn pre_indexed_registry_round_trip() {
        // Why: Confirms the OnceLock-backed slot accepts a registry and
        // returns the same symbol set on read.
        // What: Builds a registry with one entry, installs it, reads it
        // back, asserts the entry survives the round-trip.
        // Test: this test.
        let dir = tempdir().unwrap();
        let mut reg = SymbolRegistry::new(dir.path().to_path_buf());
        reg.insert(SymbolEntry::new(
            SymbolId::new("test", "preindex_marker"),
            trusty_common::symgraph::registry::SymbolKind::Function,
            "fn preindex_marker() {}".into(),
            "rust",
        ));

        // OnceLock can only be set once per process — `set_pre_indexed_registry`
        // ignores subsequent sets. The test asserts via the marker symbol so
        // ordering with sibling tests doesn't matter.
        set_pre_indexed_registry(reg);
        let got = get_pre_indexed_registry().expect("registry installed");
        let guard = got.read().unwrap();
        assert!(
            guard
                .iter()
                .any(|(id, _)| id.as_str().contains("preindex_marker")),
            "expected installed registry to contain preindex_marker"
        );
    }

    #[test]
    fn pre_index_directory_tags_assigned_file() {
        // Why: Pre-indexed lookup in GetSymbolTool filters by `assigned_file`,
        // so every entry must carry its source path.
        // What: Writes a tiny Rust source file, runs `pre_index_directory`,
        // asserts each entry's `assigned_file` matches the file written.
        // Test: this test.
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let file = src_dir.join("lib.rs");
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(b"pub fn answer() -> i32 { 42 }\n").unwrap();

        let registry = pre_index_directory(dir.path(), dir.path()).unwrap();
        assert!(
            !registry.is_empty(),
            "expected at least one symbol, got {}",
            registry.len()
        );
        for (_, entry) in registry.iter() {
            assert_eq!(
                entry.assigned_file.as_deref(),
                Some(file.as_path()),
                "entry {:?} missing assigned_file",
                entry.id
            );
        }
    }
}
