//! `check_circular_dependencies` — detect file-level import cycles (#373).
//!
//! Why: Cyclic imports hint at modeling problems. Surface them as a list
//! of SCCs (strongly connected components) so the LLM can suggest refactors.
//! What: Walks the project, builds a `file → Set<imported_file>` map by
//! resolving import strings best-effort to filesystem paths, runs Tarjan's
//! SCC algorithm via petgraph, returns SCCs of size >= 2.
//! Test: `circular_deps_smoke` ensures the tool runs without crashing on
//! a cycle-free fixture.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use petgraph::algo::tarjan_scc;
use petgraph::graphmap::DiGraphMap;
use serde_json::{Value, json};

use super::analyze_project::collect_source_files;
use crate::tools::traits::{ToolExecutor, ToolResult};

use trusty_common::symgraph::symbol::{SymbolKind, detect_language, extract_symbols};

pub struct CheckCircularDependenciesTool;

#[async_trait]
impl ToolExecutor for CheckCircularDependenciesTool {
    fn name(&self) -> &str {
        "check_circular_dependencies"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "check_circular_dependencies",
                "description": "Detect circular import cycles between source files in the project. Returns each strongly-connected component containing >1 file. Uses Tarjan's SCC over a file→imported_file graph built best-effort from each file's imports.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "root": {"type": "string", "description": "Root directory. Defaults to project root."}
                    },
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let root: PathBuf = match args.get("root").and_then(Value::as_str) {
            Some(s) => PathBuf::from(s),
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };

        let files = collect_source_files(&root);

        // Build (file → imports) map.
        let imports_per_file = collect_file_imports(&files);

        // Best-effort resolution: import name → file path.
        let resolved = resolve_imports(&imports_per_file, &files, &root);

        // Build graph keyed by file path strings.
        let mut g: DiGraphMap<&str, ()> = DiGraphMap::new();
        // Use a vector of owned strings keyed by the file path.
        let nodes: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        for n in &nodes {
            g.add_node(n.as_str());
        }
        for (from_path, to_paths) in &resolved {
            let from_s = from_path.display().to_string();
            for to_path in to_paths {
                let to_s = to_path.display().to_string();
                // Add nodes / edge by looking up the matching string in `nodes`.
                if let (Some(fnode), Some(tnode)) = (
                    nodes.iter().find(|n| **n == from_s),
                    nodes.iter().find(|n| **n == to_s),
                ) {
                    g.add_edge(fnode.as_str(), tnode.as_str(), ());
                }
            }
        }

        let sccs = tarjan_scc(&g);
        let cycles: Vec<Value> = sccs
            .into_iter()
            .filter(|c| c.len() > 1)
            .map(|c| {
                let files: Vec<String> = c.iter().map(|s| s.to_string()).collect();
                json!({"files": files, "length": files.len()})
            })
            .collect();

        let total = cycles.len();
        ToolResult::ok(json!({"cycles": cycles, "total_cycles": total}).to_string())
    }
}

/// Per-file list of import strings (the raw text of each import statement).
fn collect_file_imports(files: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    let mut out = HashMap::new();
    for path in files {
        let Some((lang, _)) = detect_language(path) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let syms = extract_symbols(&source, lang, path);
        let imports: Vec<String> = syms
            .into_iter()
            .filter(|s| matches!(s.kind, SymbolKind::Import))
            .map(|s| s.source)
            .collect();
        out.insert(path.clone(), imports);
    }
    out
}

/// Best-effort import resolution.
///
/// Why: An import like `use crate::foo::bar` should map back to
/// `src/foo/bar.rs`. We don't try to be perfect — we just look for any
/// project file whose stem appears as a path segment in the import text.
/// What: For each import string, splits on common separators (`::`, `.`,
/// `/`), strips quotes, takes terms longer than one char, and matches
/// against the file's stem. Returns a per-file `BTreeSet<PathBuf>`.
/// Test: Indirectly via `circular_deps_smoke`.
fn resolve_imports(
    imports_per_file: &HashMap<PathBuf, Vec<String>>,
    files: &[PathBuf],
    _root: &Path,
) -> HashMap<PathBuf, BTreeSet<PathBuf>> {
    // Build stem → paths.
    let mut by_stem: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for f in files {
        if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
            by_stem.entry(stem.to_string()).or_default().push(f.clone());
        }
    }

    let mut out: HashMap<PathBuf, BTreeSet<PathBuf>> = HashMap::new();
    for (file, imports) in imports_per_file {
        let mut targets: BTreeSet<PathBuf> = BTreeSet::new();
        for imp in imports {
            let cleaned = imp.replace(['"', '\'', ';', '{', '}', '(', ')', ',', '*'], " ");
            let terms: HashSet<&str> = cleaned
                .split([':', '.', '/', '\\', ' '])
                .filter(|s| s.len() > 1 && !is_keyword(s))
                .collect();
            for t in terms {
                if let Some(matches) = by_stem.get(t) {
                    for m in matches {
                        if m != file {
                            targets.insert(m.clone());
                        }
                    }
                }
            }
        }
        out.insert(file.clone(), targets);
    }
    out
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "use"
            | "mod"
            | "pub"
            | "crate"
            | "super"
            | "self"
            | "as"
            | "from"
            | "import"
            | "package"
            | "std"
            | "core"
    )
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

    #[tokio::test]
    async fn circular_deps_smoke() {
        let dir = tempdir().unwrap();
        write_tmp(dir.path(), "a.rs", "fn a() {}\n");
        write_tmp(dir.path(), "b.rs", "fn b() {}\n");
        let t = CheckCircularDependenciesTool;
        let r = t
            .execute(json!({"root": dir.path().to_string_lossy()}))
            .await;
        assert!(!r.is_error(), "{}", r.content());
        let v: Value = serde_json::from_str(r.content()).unwrap();
        assert_eq!(v["total_cycles"], 0);
    }
}
