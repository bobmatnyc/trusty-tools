//! Sorted, content-addressed symbol registry (#350).
//!
//! Why: Treating the codebase as a deterministic, content-addressed set of
//! symbols (rather than a pile of files) lets the harness round-trip
//! parse → registry → emit and produce byte-identical source for the same
//! inputs. SHA-256 hashing makes drift detectable.
//! What: `SymbolId`, `SymbolKind`, `SymbolEntry`, and `SymbolRegistry` —
//! the registry persists as canonical sorted JSON at
//! `.trusty-agents/state/symbol-registry.json`.
//! Test: `cargo test ast::registry` — covers id construction, hash
//! stability, sorted-on-insert, save/load round-trip, and stale-hash
//! detection.

use anyhow::Result;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Fully-qualified symbol identifier: `"{module_path}::{name}"`.
///
/// Why: A stable, sortable key is required for canonical (deterministic)
/// serialization. Module-qualified names disambiguate symbols that share
/// a short name across modules.
/// What: Wraps a `String`; for the root module (`main.rs`/`lib.rs`) this
/// is just `"{name}"` with no `::` prefix.
/// Test: See `test_symbol_id_new_with_module` and `test_symbol_id_root_module`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub String);

impl SymbolId {
    /// Build a SymbolId from a module path and bare name.
    ///
    /// Why: Centralize the `module::name` formatting so all callers produce
    /// identical keys — divergent formatting would silently fragment the
    /// registry.
    /// What: Returns `"{name}"` if `module_path` is empty, else
    /// `"{module_path}::{name}"`.
    /// Test: See `test_symbol_id_new_with_module`.
    pub fn new(module_path: &str, name: &str) -> Self {
        if module_path.is_empty() {
            Self(name.to_string())
        } else {
            Self(format!("{module_path}::{name}"))
        }
    }

    /// Borrow the inner string for comparison/display.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Coarse classification of a symbol entry.
///
/// Why: Downstream tooling (emitter, layout rules, test-coverage links)
/// needs a stable enum to branch on without reparsing source.
/// What: A flat enum serialized as snake_case strings.
/// Test: Indirectly via parser/emitter tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Trait,
    Impl,
    Import,
    TypeAlias,
    Const,
    Test,
    TestSuite,
    Unknown,
}

/// One symbol's full record in the registry.
///
/// Why: All facts about a symbol (source text, language, deps, hash,
/// optional file override, optional test→prod link) live in one row so
/// the registry is the single source of truth.
/// What: Plain serde struct; `dependencies` is a `BTreeSet` for sorted
/// canonical output.
/// Test: Round-trip covered by `test_registry_save_load_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub source: String,
    /// SHA-256 hex of `source` (stable across platforms).
    pub content_hash: String,
    /// Language tag: `"rust"` | `"python"` | `"javascript"` | `"go"`.
    pub language: String,
    /// Symbols this entry depends on (call edges, type refs, etc.).
    /// Sorted for determinism.
    pub dependencies: BTreeSet<SymbolId>,
    /// Optional explicit file assignment; `None` means "use layout rules".
    pub assigned_file: Option<PathBuf>,
    /// For `SymbolKind::Test`: the production symbol this test covers.
    pub test_covers: Option<SymbolId>,
}

impl SymbolEntry {
    /// Construct an entry, computing the content hash from `source`.
    ///
    /// Why: Forces every entry to enter the registry with a hash — never
    /// `Default::default()`-ed to empty.
    /// What: Sets `content_hash = SHA-256(source)`, leaves `dependencies`
    /// empty, `assigned_file = None`, `test_covers = None`.
    /// Test: See `test_content_hash_stable`.
    pub fn new(id: SymbolId, kind: SymbolKind, source: String, language: &str) -> Self {
        let content_hash = SymbolRegistry::content_hash(&source);
        Self {
            id,
            kind,
            source,
            content_hash,
            language: language.to_string(),
            dependencies: BTreeSet::new(),
            assigned_file: None,
            test_covers: None,
        }
    }
}

/// On-disk registry envelope (versioned for schema migrations).
#[derive(Debug, Serialize, Deserialize)]
struct RegistryState {
    version: u32,
    /// Always serialized in sorted order.
    symbols: Vec<SymbolEntry>,
}

/// Sorted, content-addressed in-memory store of symbols for one project.
///
/// Why: Acts as the canonical model the emitter projects to disk. Order
/// must be deterministic so the JSON encoding is byte-stable for the
/// same logical content.
/// What: Wraps an `IndexMap<SymbolId, SymbolEntry>` and re-sorts on every
/// insert/load.
/// Test: `test_registry_sorted_on_insert`, `test_registry_save_load_roundtrip`.
pub struct SymbolRegistry {
    entries: IndexMap<SymbolId, SymbolEntry>,
    pub project_root: PathBuf,
}

impl SymbolRegistry {
    const REGISTRY_PATH: &'static str = ".trusty-agents/state/symbol-registry.json";
    const VERSION: u32 = 1;

    /// New empty registry rooted at `project_root`.
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            entries: IndexMap::new(),
            project_root,
        }
    }

    /// SHA-256 hex digest of source text.
    ///
    /// Why: Stable, platform-independent fingerprint for change detection.
    /// What: `sha2::Sha256` over UTF-8 bytes, lowercase hex output.
    /// Test: `test_content_hash_stable`.
    pub fn content_hash(source: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(source.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Insert or replace a symbol. Registry remains sorted by `SymbolId`.
    ///
    /// Why: Sorted-on-insert keeps iteration and serialization deterministic
    /// without callers needing to remember to sort.
    /// What: Inserts into `IndexMap`, then `sort_keys()`.
    /// Test: `test_registry_sorted_on_insert`.
    pub fn insert(&mut self, entry: SymbolEntry) {
        self.entries.insert(entry.id.clone(), entry);
        self.entries.sort_keys();
    }

    /// Remove a symbol by id. Returns the removed entry if any.
    pub fn remove(&mut self, id: &SymbolId) -> Option<SymbolEntry> {
        self.entries.shift_remove(id)
    }

    /// Look up a symbol by id.
    pub fn get(&self, id: &SymbolId) -> Option<&SymbolEntry> {
        self.entries.get(id)
    }

    /// Iterate over `(id, entry)` pairs in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&SymbolId, &SymbolEntry)> {
        self.entries.iter()
    }

    /// Number of symbols in the registry.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the registry has no symbols.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return the ids of symbols whose stored hash doesn't match the
    /// recomputed hash of their `source`.
    ///
    /// Why: Detects out-of-band edits or persistence corruption.
    /// What: Filters `entries` by `content_hash != SHA-256(source)`.
    /// Test: `test_verify_hashes_detects_mismatch`.
    pub fn verify_hashes(&self) -> Vec<SymbolId> {
        self.entries
            .iter()
            .filter(|(_, e)| e.content_hash != Self::content_hash(&e.source))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Absolute path to the on-disk registry file for this project.
    pub fn registry_path(&self) -> PathBuf {
        self.project_root.join(Self::REGISTRY_PATH)
    }

    /// Load the registry from `.trusty-agents/state/symbol-registry.json`.
    ///
    /// Why: Persistence allows the parse and emit phases to run as
    /// separate CLI invocations.
    /// What: Returns an empty registry if the file is missing; otherwise
    /// parses the JSON envelope and re-sorts.
    /// Test: `test_registry_save_load_roundtrip`.
    pub fn load(project_root: &Path) -> Result<Self> {
        let path = project_root.join(Self::REGISTRY_PATH);
        if !path.exists() {
            return Ok(Self::new(project_root.to_path_buf()));
        }
        let json = std::fs::read_to_string(&path)?;
        let state: RegistryState = serde_json::from_str(&json)?;
        let mut registry = Self::new(project_root.to_path_buf());
        for entry in state.symbols {
            registry.entries.insert(entry.id.clone(), entry);
        }
        registry.entries.sort_keys();
        Ok(registry)
    }

    /// Persist the registry as canonical sorted JSON.
    ///
    /// Why: Deterministic bytes for the same logical content keep diffs
    /// minimal in version control.
    /// What: Writes a versioned envelope; creates parent dirs as needed.
    /// Test: `test_registry_save_load_roundtrip`.
    pub fn save(&self) -> Result<()> {
        let path = self.registry_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let symbols: Vec<SymbolEntry> = self.entries.values().cloned().collect();
        let state = RegistryState {
            version: Self::VERSION,
            symbols,
        };
        let json = serde_json::to_string_pretty(&state)?;
        std::fs::write(&path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_symbol_id_new_with_module() {
        let id = SymbolId::new("api::handlers", "process_request");
        assert_eq!(id.as_str(), "api::handlers::process_request");
    }

    #[test]
    fn test_symbol_id_root_module() {
        let id = SymbolId::new("", "main");
        assert_eq!(id.as_str(), "main");
    }

    #[test]
    fn test_content_hash_stable() {
        let h1 = SymbolRegistry::content_hash("fn foo() {}");
        let h2 = SymbolRegistry::content_hash("fn foo() {}");
        assert_eq!(h1, h2);
        assert_ne!(h1, SymbolRegistry::content_hash("fn bar() {}"));
    }

    #[test]
    fn test_registry_sorted_on_insert() {
        let tmp = TempDir::new().unwrap();
        let mut reg = SymbolRegistry::new(tmp.path().to_path_buf());
        let e_z = SymbolEntry::new(
            SymbolId::new("", "z_func"),
            SymbolKind::Function,
            "fn z_func() {}".into(),
            "rust",
        );
        let e_a = SymbolEntry::new(
            SymbolId::new("", "a_func"),
            SymbolKind::Function,
            "fn a_func() {}".into(),
            "rust",
        );
        reg.insert(e_z);
        reg.insert(e_a);
        let ids: Vec<&str> = reg.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["a_func", "z_func"]);
    }

    #[test]
    fn test_registry_save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut reg = SymbolRegistry::new(tmp.path().to_path_buf());
        reg.insert(SymbolEntry::new(
            SymbolId::new("mod", "foo"),
            SymbolKind::Function,
            "fn foo() {}".into(),
            "rust",
        ));
        reg.save().unwrap();
        let loaded = SymbolRegistry::load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.get(&SymbolId::new("mod", "foo")).is_some());
    }

    #[test]
    fn test_verify_hashes_detects_mismatch() {
        let tmp = TempDir::new().unwrap();
        let mut reg = SymbolRegistry::new(tmp.path().to_path_buf());
        let mut entry = SymbolEntry::new(
            SymbolId::new("", "foo"),
            SymbolKind::Function,
            "fn foo() {}".into(),
            "rust",
        );
        entry.content_hash = "badhash".into();
        reg.insert(entry);
        let stale = reg.verify_hashes();
        assert_eq!(stale.len(), 1);
    }
}
