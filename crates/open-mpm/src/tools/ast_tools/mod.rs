//! AST-native tool surface (#347).
//!
//! Why: Replaces whole-file `write_file` rewrites with surgical, AST-aware
//! edits. Every tool returns structured JSON so the LLM can reason about
//! diffs, syntax errors, and patch IDs without parsing free-form text.
//! What: Six tools — `get_symbol`, `edit_symbol`, `insert_symbol`,
//! `add_import`, `validate_syntax`, `apply_patch` — each implementing
//! `ToolExecutor`. Pending edits land in a `PatchStore` keyed by uuid that
//! is owned by the tool bundle (no process-global state); `apply_patch` is
//! the only tool that mutates disk. Public helper `ast_native_tools()`
//! constructs a fresh store and returns the six tools sharing it.
//! Test: Each tool has a happy-path unit test below; the in-memory
//! `PatchStore` is exercised end-to-end by `edit_then_apply_round_trips`.

use std::sync::Arc;

use crate::tools::traits::ToolExecutor;

mod apply_patch;
mod edit_insert;
mod get_symbol;
mod import_validate;
mod patch_store;

pub use apply_patch::ApplyPatchTool;
pub use edit_insert::{EditSymbolTool, InsertSymbolTool};
pub use get_symbol::GetSymbolTool;
pub use import_validate::{AddImportTool, ValidateSyntaxTool};
pub use patch_store::{PatchStore, new_patch_store};

/// Build the canonical 6-tool AST-native bundle.
///
/// Why: Single registration call keeps `[tools] ast_native = true` ergonomic
/// for callers (in-process runner, CTRL, future agents). Each call now
/// allocates a fresh `PatchStore`, so different bundles (e.g. concurrent
/// agent runs or per-test bundles) cannot leak pending patches across one
/// another the way the former process-global static did.
/// What: Constructs one `PatchStore`, clones it into every producer / consumer
/// tool, and returns the six tools as `Vec<Arc<dyn ToolExecutor>>`.
/// Test: `ast_native_tools_returns_six` — names match and length is 6;
/// `edit_then_apply_round_trips` exercises a bundle end-to-end.
pub fn ast_native_tools() -> Vec<Arc<dyn ToolExecutor>> {
    let patch_store = new_patch_store();
    vec![
        Arc::new(GetSymbolTool),
        Arc::new(EditSymbolTool::new(patch_store.clone())),
        Arc::new(InsertSymbolTool::new(patch_store.clone())),
        Arc::new(AddImportTool),
        Arc::new(ValidateSyntaxTool),
        Arc::new(ApplyPatchTool::new(patch_store)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn write_tmp(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn ast_native_tools_returns_six() {
        let v = ast_native_tools();
        assert_eq!(v.len(), 6);
        let names: Vec<&str> = v.iter().map(|t| t.name()).collect();
        for n in [
            "get_symbol",
            "edit_symbol",
            "insert_symbol",
            "add_import",
            "validate_syntax",
            "apply_patch",
        ] {
            assert!(names.contains(&n), "missing tool {n} in {names:?}");
        }
    }

    #[tokio::test]
    async fn get_symbol_returns_source() {
        let dir = tempdir().unwrap();
        let p = write_tmp(dir.path(), "x.rs", "fn foo() -> i32 { 7 }\n");
        let t = GetSymbolTool;
        let r = t
            .execute(json!({"file": p.to_string_lossy(), "name": "foo"}))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["name"], "foo");
        assert!(v["source"].as_str().unwrap().contains("7"));
    }

    #[tokio::test]
    async fn validate_syntax_tool_reports_error() {
        let t = ValidateSyntaxTool;
        let r = t
            .execute(json!({"file": "x.rs", "source": "fn main( {"}))
            .await;
        assert!(!r.is_error());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["valid"], false);
        assert!(!v["errors"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn edit_then_apply_round_trips() {
        // Why: Producer and consumer tools must share one store. Each test
        // now owns its own store (no global `clear_store()` needed).
        let store = new_patch_store();
        let dir = tempdir().unwrap();
        let p = write_tmp(dir.path(), "x.rs", "fn foo() -> i32 { 1 }\n");
        let edit = EditSymbolTool::new(store.clone());
        let r = edit
            .execute(json!({
                "file": p.to_string_lossy(),
                "name": "foo",
                "new_source": "fn foo() -> i32 { 99 }"
            }))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        let id = v["patch_id"].as_str().unwrap().to_string();
        assert_eq!(v["status"], "pending");

        // File should be unchanged on disk before apply.
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(on_disk.contains(" 1 "));

        let apply = ApplyPatchTool::new(store);
        let r2 = apply.execute(json!({"patch_id": id})).await;
        assert!(!r2.is_error(), "{}", r2.content());

        let after = std::fs::read_to_string(&p).unwrap();
        assert!(
            after.contains("99"),
            "file should contain new body: {after}"
        );
    }

    #[tokio::test]
    async fn separate_bundles_have_isolated_patch_stores() {
        // Why: Regression guard for #252 — two bundles built via
        // `ast_native_tools()` must NOT share a patch store, so a pending
        // patch produced in bundle A cannot be applied by bundle B. This
        // would have been impossible to assert against the old global static.
        // What: Stage an edit in bundle A, then try to apply its patch_id via
        // bundle B's apply_patch tool and expect a "no pending patch" error.
        // Test: this test.
        let dir = tempdir().unwrap();
        let p = write_tmp(dir.path(), "x.rs", "fn foo() -> i32 { 1 }\n");
        let bundle_a = ast_native_tools();
        let bundle_b = ast_native_tools();

        // Names from the schema: index 1 = edit_symbol, index 5 = apply_patch.
        let edit_a = &bundle_a[1];
        assert_eq!(edit_a.name(), "edit_symbol");
        let apply_b = &bundle_b[5];
        assert_eq!(apply_b.name(), "apply_patch");

        let r = edit_a
            .execute(json!({
                "file": p.to_string_lossy(),
                "name": "foo",
                "new_source": "fn foo() -> i32 { 99 }"
            }))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        let id = v["patch_id"].as_str().unwrap().to_string();

        let r2 = apply_b.execute(json!({"patch_id": id})).await;
        assert!(
            r2.is_error(),
            "bundle B must not see bundle A's pending patch"
        );
    }

    #[tokio::test]
    #[serial_test::serial(pre_indexed_registry)]
    async fn get_symbol_uses_pre_indexed_registry() {
        // Why: When a project has been pre-indexed, GetSymbolTool must
        // serve the lookup from the registry without re-parsing the file.
        // What: Builds a registry holding one symbol whose `assigned_file`
        // points at a synthetic path that does NOT exist on disk. If the
        // tool falls through to `list_symbols`, the read will fail; if it
        // serves from the registry, the response will reference the
        // pre-indexed `source_of_truth`.
        // Test: this test.
        use crate::ast::{SymbolEntry, SymbolId, SymbolRegistry};

        let dir = tempdir().unwrap();
        let phantom = dir.path().join("nonexistent.rs");

        let mut reg = SymbolRegistry::new(dir.path().to_path_buf());
        let mut entry = SymbolEntry::new(
            SymbolId::new("phantom_mod", "preindex_only_symbol"),
            trusty_common::symgraph::registry::SymbolKind::Function,
            "fn preindex_only_symbol() -> i32 { 7 }".into(),
            "rust",
        );
        entry.assigned_file = Some(phantom.clone());
        reg.insert(entry);
        crate::ast::set_pre_indexed_registry(reg);

        let t = GetSymbolTool;
        let r = t
            .execute(json!({
                "file": phantom.to_string_lossy(),
                "name": "preindex_only_symbol",
            }))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["name"], "preindex_only_symbol");
        assert_eq!(v["source_of_truth"], "pre_indexed_registry");
        assert!(v["source"].as_str().unwrap().contains("7"));
    }

    #[tokio::test]
    async fn add_import_tool_applies_immediately() {
        let dir = tempdir().unwrap();
        let p = write_tmp(dir.path(), "x.rs", "fn main() {}\n");
        let t = AddImportTool;
        let r = t
            .execute(json!({
                "file": p.to_string_lossy(),
                "import_stmt": "use std::fs;"
            }))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["applied"], true);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("use std::fs;"));
    }
}
