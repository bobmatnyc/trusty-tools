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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ast::editor::{
    Patch, add_import as do_add_import, apply_patch as do_apply_patch, insert_after_symbol,
    replace_symbol, validate_syntax,
};
use crate::ast::kg::SymbolGraph;
use crate::ast::symbol::{detect_language, list_symbols};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Shared store of pending patches owned by a tool bundle.
///
/// Why: AST tools split "produce a diff" (tool call N) from "apply the diff"
/// (tool call N+1) so the LLM can review the change before committing. The
/// orchestrator routes both calls into the same address space, so an
/// in-process map keyed by uuid is sufficient. Threading the store through
/// the tool instances (rather than a process-global `Lazy`) gives each test
/// and each tool bundle a fresh, isolated address space — eliminating the
/// inter-test contamination that the global static caused.
/// What: `Arc<Mutex<HashMap<String, Patch>>>` constructed once by
/// `ast_native_tools()` (or directly by tests) and cloned into every tool
/// that participates in the produce-then-apply protocol.
/// Test: `edit_then_apply_round_trips`.
pub type PatchStore = Arc<Mutex<HashMap<String, Patch>>>;

/// Construct an empty `PatchStore` ready to be cloned into tool instances.
///
/// Why: Single canonical constructor so call sites (production
/// `ast_native_tools` and per-test bundles) never differ in how the inner
/// mutex / hashmap is initialised.
/// What: Returns `Arc::new(Mutex::new(HashMap::new()))`.
/// Test: Implicit in every test that builds a tool with `new_patch_store()`.
pub fn new_patch_store() -> PatchStore {
    Arc::new(Mutex::new(HashMap::new()))
}

fn store_patch(store: &PatchStore, p: Patch) -> String {
    let id = p.id.clone();
    // Why: poisoned mutexes here would mean a panic-during-edit corrupted the
    // store — recoverable by treating the poisoned lock as still-usable for a
    // fresh insert. We deliberately `unwrap_or_else(PoisonError::into_inner)`
    // to keep the tool surface infallible.
    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    guard.insert(id.clone(), p);
    id
}

fn take_patch(store: &PatchStore, id: &str) -> Option<Patch> {
    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    guard.remove(id)
}

// ──────────────────────────────────────────────────────────────────────────
// get_symbol
// ──────────────────────────────────────────────────────────────────────────

/// `get_symbol` — return a named symbol's source plus call-graph context.
pub struct GetSymbolTool;

#[async_trait]
impl ToolExecutor for GetSymbolTool {
    fn name(&self) -> &str {
        "get_symbol"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "get_symbol",
                "description": "Locate a named symbol (function/struct/class/etc.) in a source file and return its source code along with callers/callees from the file's symbol graph. Use this before editing to understand a symbol's role.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string", "description": "Path to a source file (.rs/.py/.js/.go)."},
                        "name": {"type": "string", "description": "Exact symbol name (case-sensitive)."}
                    },
                    "required": ["file", "name"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("get_symbol: missing 'file'");
        };
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("get_symbol: missing 'name'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "lookup".into(),
            detail: format!("symbol `{name}` in {file}"),
        });
        let path = PathBuf::from(file);

        // #347 follow-up: Consult the pre-indexed registry first.
        //
        // Why: Workflow runs over existing codebases pre-populate a global
        // SymbolRegistry before the phase loop starts. Hitting that cache
        // avoids a fresh disk read + tree-sitter parse on every `get_symbol`
        // call against an already-known file.
        // What: When the registry is installed AND it has at least one entry
        // tagged with this file via `assigned_file`, build a JSON response
        // from the registry entry directly (line numbers default to 0 — the
        // registry is line-agnostic; callers that need ranges should call a
        // future `get_symbol_lines` helper). Fall back to on-demand parse
        // when the file isn't in the index (newly created during the run).
        // Test: `get_symbol_uses_pre_indexed_registry` below.
        if let Some(registry_arc) = crate::ast::get_pre_indexed_registry()
            && let Ok(registry) = registry_arc.read()
        {
            let mut hit: Option<&crate::ast::SymbolEntry> = None;
            for (id, entry) in registry.iter() {
                if entry.assigned_file.as_deref() == Some(path.as_path())
                    && (id.as_str() == name || id.as_str().ends_with(&format!("::{name}")))
                {
                    hit = Some(entry);
                    break;
                }
            }
            if let Some(entry) = hit {
                let out = json!({
                    "name": name,
                    "kind": entry.kind,
                    "file": file,
                    "start_line": 0,
                    "end_line": 0,
                    "source": entry.source,
                    "callers": [],
                    "callees": [],
                    "source_of_truth": "pre_indexed_registry",
                });
                return ToolResult::ok(out.to_string());
            }
        }

        // Fall back to on-demand parse (for files created during the run, or
        // when no pre-index was performed).
        let symbols = match list_symbols(&path) {
            Ok(s) => s,
            Err(e) => return ToolResult::err(format!("get_symbol: {e}")),
        };
        let Some(sym) = symbols.into_iter().find(|s| s.name == name) else {
            return ToolResult::err(format!("get_symbol: '{name}' not found in {file}"));
        };

        // KG context. Failure to build the graph is non-fatal — we still
        // return the symbol so the LLM has something to work with.
        let (callers, callees) = match SymbolGraph::build_from_file(&path) {
            Ok(g) => {
                let callers: Vec<Value> = g
                    .callers_of(name)
                    .into_iter()
                    .map(|n| json!({"name": n.name, "kind": n.kind, "start_line": n.start_line}))
                    .collect();
                let callees: Vec<Value> = g
                    .callees_of(name)
                    .into_iter()
                    .map(|n| json!({"name": n.name, "kind": n.kind, "start_line": n.start_line}))
                    .collect();
                (callers, callees)
            }
            Err(_) => (Vec::new(), Vec::new()),
        };

        let out = json!({
            "name": sym.name,
            "kind": sym.kind,
            "file": file,
            "start_line": sym.start_line,
            "end_line": sym.end_line,
            "source": sym.source,
            "callers": callers,
            "callees": callees,
        });
        ToolResult::ok(out.to_string())
    }
}

// ──────────────────────────────────────────────────────────────────────────
// edit_symbol
// ──────────────────────────────────────────────────────────────────────────

/// `edit_symbol` — splice replacement source into a named symbol's range.
///
/// Why: Pending patches need to land in a store that `apply_patch` can later
/// drain, but the store must not be process-global (see `PatchStore` docs).
/// Holding an `Arc<PatchStore>` lets every tool in a bundle share one store
/// while different bundles (tests, separate agent runs) stay isolated.
/// What: Carries a clone of the bundle's `PatchStore` and writes pending
/// patches into it on each `execute()` call.
/// Test: `edit_then_apply_round_trips`.
pub struct EditSymbolTool {
    patch_store: PatchStore,
}

impl EditSymbolTool {
    /// Build with an explicit `PatchStore` clone.
    ///
    /// Why: Lets bundle constructors (`ast_native_tools`) and tests inject the
    /// shared store explicitly rather than reaching for a global.
    /// What: Stores the clone; `execute()` writes pending patches into it.
    /// Test: Used by every test that constructs `EditSymbolTool`.
    pub fn new(patch_store: PatchStore) -> Self {
        Self { patch_store }
    }
}

#[async_trait]
impl ToolExecutor for EditSymbolTool {
    fn name(&self) -> &str {
        "edit_symbol"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "edit_symbol",
                "description": "Replace a named symbol's full source with `new_source`. Validates syntax and stages the change as a pending patch (returned `patch_id`). Call `apply_patch` to commit. Disk is NOT modified by this call.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "name": {"type": "string", "description": "Symbol name to replace."},
                        "new_source": {"type": "string", "description": "Full replacement source for the symbol."}
                    },
                    "required": ["file", "name", "new_source"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("edit_symbol: missing 'file'");
        };
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("edit_symbol: missing 'name'");
        };
        let Some(new_source) = args.get("new_source").and_then(Value::as_str) else {
            return ToolResult::err("edit_symbol: missing 'new_source'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "edit".into(),
            detail: format!("`{name}` in {file}"),
        });
        match replace_symbol(Path::new(file), name, new_source) {
            Ok(p) => {
                let id = p.id.clone();
                let diff = p.diff.clone();
                store_patch(&self.patch_store, p);
                ToolResult::ok(
                    json!({
                        "patch_id": id,
                        "diff": diff,
                        "status": "pending"
                    })
                    .to_string(),
                )
            }
            Err(e) => ToolResult::err(format!("edit_symbol: {e}")),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// insert_symbol
// ──────────────────────────────────────────────────────────────────────────

/// `insert_symbol` — insert a new symbol after an anchor symbol.
///
/// Why: Like `EditSymbolTool`, must stage a pending patch into the bundle's
/// shared `PatchStore` without touching a process-global static.
/// What: Carries a clone of the bundle's `PatchStore`.
/// Test: `edit_then_apply_round_trips` and the insert path is exercised by
/// integration tests in `ast::editor`.
pub struct InsertSymbolTool {
    patch_store: PatchStore,
}

impl InsertSymbolTool {
    /// Build with an explicit `PatchStore` clone.
    ///
    /// Why: Mirror of `EditSymbolTool::new` — explicit injection, no globals.
    /// What: Stores the clone.
    /// Test: Used by `ast_native_tools` and tests that build the tool directly.
    pub fn new(patch_store: PatchStore) -> Self {
        Self { patch_store }
    }
}

#[async_trait]
impl ToolExecutor for InsertSymbolTool {
    fn name(&self) -> &str {
        "insert_symbol"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "insert_symbol",
                "description": "Insert new source code (a function, struct, etc.) immediately after the named anchor symbol. Validates syntax and stages a pending patch. Disk is NOT modified.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "after": {"type": "string", "description": "Anchor symbol name; new code is inserted after its closing byte."},
                        "source": {"type": "string", "description": "Source code to insert."}
                    },
                    "required": ["file", "after", "source"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("insert_symbol: missing 'file'");
        };
        let Some(after) = args.get("after").and_then(Value::as_str) else {
            return ToolResult::err("insert_symbol: missing 'after'");
        };
        let Some(source) = args.get("source").and_then(Value::as_str) else {
            return ToolResult::err("insert_symbol: missing 'source'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "insert".into(),
            detail: format!("after `{after}` in {file}"),
        });
        match insert_after_symbol(Path::new(file), after, source) {
            Ok(p) => {
                let id = p.id.clone();
                let diff = p.diff.clone();
                store_patch(&self.patch_store, p);
                ToolResult::ok(
                    json!({
                        "patch_id": id,
                        "diff": diff,
                        "status": "pending"
                    })
                    .to_string(),
                )
            }
            Err(e) => ToolResult::err(format!("insert_symbol: {e}")),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// add_import
// ──────────────────────────────────────────────────────────────────────────

/// `add_import` — language-aware import insertion. Applied immediately
/// (low-risk, side-effect free).
pub struct AddImportTool;

#[async_trait]
impl ToolExecutor for AddImportTool {
    fn name(&self) -> &str {
        "add_import"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "add_import",
                "description": "Add an import statement to a source file at the language-appropriate location (after the last existing import, or at the top of the file). Duplicate imports are skipped. Applied immediately to disk.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "import_stmt": {"type": "string", "description": "Full import line, e.g. `use std::fs;` or `import os`."}
                    },
                    "required": ["file", "import_stmt"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("add_import: missing 'file'");
        };
        let Some(import_stmt) = args.get("import_stmt").and_then(Value::as_str) else {
            return ToolResult::err("add_import: missing 'import_stmt'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "import".into(),
            detail: format!("{import_stmt} → {file}"),
        });
        match do_add_import(Path::new(file), import_stmt) {
            Ok(p) => {
                if p.original == p.modified {
                    return ToolResult::ok(
                        json!({
                            "file": file,
                            "import_stmt": import_stmt,
                            "applied": false,
                            "reason": "import already present"
                        })
                        .to_string(),
                    );
                }
                if let Err(e) = do_apply_patch(&p) {
                    return ToolResult::err(format!("add_import: failed to write: {e}"));
                }
                ToolResult::ok(
                    json!({
                        "file": file,
                        "import_stmt": import_stmt,
                        "applied": true,
                        "diff": p.diff
                    })
                    .to_string(),
                )
            }
            Err(e) => ToolResult::err(format!("add_import: {e}")),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// validate_syntax
// ──────────────────────────────────────────────────────────────────────────

/// `validate_syntax` — parse a source string and report any syntax errors.
pub struct ValidateSyntaxTool;

#[async_trait]
impl ToolExecutor for ValidateSyntaxTool {
    fn name(&self) -> &str {
        "validate_syntax"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "validate_syntax",
                "description": "Parse `source` using the language detected from `file`'s extension. Returns {valid, errors}. Useful for sanity-checking generated code before writing it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string", "description": "Path used only for language detection by extension."},
                        "source": {"type": "string"}
                    },
                    "required": ["file", "source"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("validate_syntax: missing 'file'");
        };
        let Some(source) = args.get("source").and_then(Value::as_str) else {
            return ToolResult::err("validate_syntax: missing 'source'");
        };
        let Some((lang, _)) = detect_language(Path::new(file)) else {
            return ToolResult::err(format!("validate_syntax: unsupported extension on {file}"));
        };
        match validate_syntax(source, lang) {
            Ok(()) => {
                crate::events::emit(crate::events::Event::AstOperation {
                    session_id: String::new(),
                    op: "validate".into(),
                    detail: format!("{file} → OK"),
                });
                ToolResult::ok(json!({"valid": true, "errors": []}).to_string())
            }
            Err(e) => {
                crate::events::emit(crate::events::Event::AstOperation {
                    session_id: String::new(),
                    op: "validate".into(),
                    detail: format!("{file} → error: {e}"),
                });
                ToolResult::ok(json!({"valid": false, "errors": [e]}).to_string())
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// apply_patch
// ──────────────────────────────────────────────────────────────────────────

/// `apply_patch` — commit a pending patch to disk.
///
/// Why: Reads from the bundle's shared `PatchStore` to find a patch produced
/// by an earlier `edit_symbol` / `insert_symbol` call. Must use the *same*
/// store the producer wrote to, hence the injected `Arc`.
/// What: Carries a clone of the bundle's `PatchStore`.
/// Test: `edit_then_apply_round_trips`.
pub struct ApplyPatchTool {
    patch_store: PatchStore,
}

impl ApplyPatchTool {
    /// Build with an explicit `PatchStore` clone.
    ///
    /// Why: Producers and `apply_patch` must reference the same store.
    /// `ast_native_tools` and any test that exercises the produce-then-apply
    /// flow constructs the store once and passes the clone here.
    /// What: Stores the clone.
    /// Test: `edit_then_apply_round_trips`.
    pub fn new(patch_store: PatchStore) -> Self {
        Self { patch_store }
    }
}

#[async_trait]
impl ToolExecutor for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Commit a pending patch (created by `edit_symbol` or `insert_symbol`) to disk. The patch is consumed (one-shot).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch_id": {"type": "string"}
                    },
                    "required": ["patch_id"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("patch_id").and_then(Value::as_str) else {
            return ToolResult::err("apply_patch: missing 'patch_id'");
        };
        let Some(patch) = take_patch(&self.patch_store, id) else {
            return ToolResult::err(format!("apply_patch: no pending patch with id '{id}'"));
        };
        let lines_changed = patch
            .diff
            .lines()
            .filter(|l| {
                (l.starts_with('+') && !l.starts_with("+++"))
                    || (l.starts_with('-') && !l.starts_with("---"))
            })
            .count();
        let file = patch.file.clone();
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "patch".into(),
            detail: format!("{lines_changed} line(s) → {}", file.display()),
        });
        match do_apply_patch(&patch) {
            Ok(()) => ToolResult::ok(
                json!({
                    "file": file.display().to_string(),
                    "lines_changed": lines_changed,
                    "status": "applied"
                })
                .to_string(),
            ),
            Err(e) => ToolResult::err(format!("apply_patch: {e}")),
        }
    }
}

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
    use std::io::Write;
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
        assert!(v["errors"].as_array().unwrap().len() >= 1);
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
