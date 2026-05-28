//! Shared pending-patch store for the AST-native tool bundle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ast::editor::Patch;

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

/// Insert a pending patch into the store, returning its id.
///
/// Why: Producer tools (`edit_symbol`, `insert_symbol`) stage patches here for
/// a later `apply_patch` call to drain.
/// What: Inserts `p` keyed by `p.id`, returning the id. Recovers from a
/// poisoned mutex to keep the tool surface infallible.
/// Test: `edit_then_apply_round_trips`.
pub(super) fn store_patch(store: &PatchStore, p: Patch) -> String {
    let id = p.id.clone();
    // Why: poisoned mutexes here would mean a panic-during-edit corrupted the
    // store — recoverable by treating the poisoned lock as still-usable for a
    // fresh insert. We deliberately `unwrap_or_else(PoisonError::into_inner)`
    // to keep the tool surface infallible.
    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    guard.insert(id.clone(), p);
    id
}

/// Remove and return a pending patch by id.
///
/// Why: `apply_patch` consumes a staged patch one-shot so a stale id cannot be
/// applied twice.
/// What: Removes the entry keyed by `id`, returning `None` if absent. Recovers
/// from a poisoned mutex.
/// Test: `edit_then_apply_round_trips`.
pub(super) fn take_patch(store: &PatchStore, id: &str) -> Option<Patch> {
    let mut guard = store.lock().unwrap_or_else(|e| e.into_inner());
    guard.remove(id)
}
