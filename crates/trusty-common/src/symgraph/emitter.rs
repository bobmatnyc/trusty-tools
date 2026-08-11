//! Deterministic registry → source-file emitter (#350).
//!
//! Why: Round-tripping registry → files must be byte-stable for the same
//! logical content. Determinism comes from sorted file order, topological
//! sort with lexicographic tie-breaking, and sorted import generation.
//! What: `LayoutRules`, `assign_file`, `emit`, `apply_emit`.
//! Test: `test_assign_file_*`, `test_emit_is_deterministic`.

use super::registry::{SymbolEntry, SymbolId, SymbolKind, SymbolRegistry};
use super::strategy::EmitStrategy;
use anyhow::Result;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors produced by the emitter's internal graph operations.
///
/// Why: A dedicated error type lets callers distinguish a cycle in the
/// dependency graph from generic I/O errors when they care to.
/// What: One variant today (`Cycle`) carrying the offending `SymbolId`.
#[derive(Debug, Error)]
pub enum EmitError {
    /// A dependency cycle was detected during topological sort.
    #[error("dependency cycle detected at symbol `{0}`")]
    Cycle(SymbolId),
}

/// Knobs controlling how the emitter projects symbols onto file paths.
///
/// Why: Different projects pick different roots (`src` vs `lib`); we keep
/// the rules in one struct so changing them doesn't ripple through the
/// emitter logic.
/// What: `max_file_lines` is a soft hint reserved for future
/// auto-splitting; `src_root` is the directory under which generated
/// files land.
pub struct LayoutRules {
    pub max_file_lines: usize,
    pub src_root: String,
}

impl Default for LayoutRules {
    fn default() -> Self {
        Self {
            max_file_lines: 500,
            src_root: "src".to_string(),
        }
    }
}

// INTENT: Deterministically map a SymbolId to a file path based on module structure.
pub fn assign_file(id: &SymbolId, src_root: &str) -> PathBuf {
    let s = id.as_str();
    if let Some(pos) = s.rfind("::") {
        let module_path = &s[..pos];
        let path_part = module_path.replace("::", "/");
        PathBuf::from(format!("{src_root}/{path_part}.rs"))
    } else {
        PathBuf::from(format!("{src_root}/main.rs"))
    }
}

// INTENT: Topologically sort symbol IDs so callees precede callers, with deterministic tie-breaking.
pub(crate) fn topological_sort(
    ids: &[SymbolId],
    registry: &SymbolRegistry,
) -> std::result::Result<Vec<SymbolId>, EmitError> {
    let mut sorted_ids: Vec<&SymbolId> = ids.iter().collect();
    sorted_ids.sort();

    let id_set: HashSet<&SymbolId> = sorted_ids.iter().copied().collect();

    let mut graph: DiGraph<SymbolId, ()> = DiGraph::new();
    let mut node_for: HashMap<SymbolId, NodeIndex> = HashMap::new();
    for id in &sorted_ids {
        let idx = graph.add_node((*id).clone());
        node_for.insert((*id).clone(), idx);
    }

    for id in &sorted_ids {
        if let Some(entry) = registry.get(id) {
            let mut deps: Vec<&SymbolId> = entry
                .dependencies
                .iter()
                .filter(|d| id_set.contains(*d))
                .collect();
            deps.sort();
            let dependent_idx = node_for[*id];
            for dep in deps {
                let dep_idx = node_for[dep];
                graph.add_edge(dep_idx, dependent_idx, ());
            }
        }
    }

    let order = petgraph::algo::toposort(&graph, None).map_err(|cycle| {
        let offending = graph[cycle.node_id()].clone();
        EmitError::Cycle(offending)
    })?;

    Ok(order.into_iter().map(|idx| graph[idx].clone()).collect())
}

// INTENT: Generate sorted, deduplicated import lines from cross-module dependencies.
fn generate_imports(symbols: &[&SymbolEntry], language: &str) -> Vec<String> {
    let mut imports: HashSet<String> = HashSet::new();
    for entry in symbols {
        for dep in &entry.dependencies {
            let dep_str = dep.as_str();
            if dep_str.contains("::") || dep_str.contains('.') {
                let import_line = match language {
                    "rust" => format!("use {dep_str};"),
                    "python" => {
                        if dep_str.contains('.') {
                            let parts: Vec<&str> = dep_str.rsplitn(2, '.').collect();
                            format!("from {} import {}", parts[1], parts[0])
                        } else {
                            format!("import {dep_str}")
                        }
                    }
                    "javascript" => format!("// import {dep_str}"),
                    "go" => format!("// import \"{dep_str}\""),
                    _ => continue,
                };
                imports.insert(import_line);
            }
        }
    }
    let mut sorted: Vec<String> = imports.into_iter().collect();
    sorted.sort();
    sorted
}

// INTENT: Detect programming language from file extension.
fn detect_language(file: &Path) -> &'static str {
    file.extension()
        .and_then(|e| e.to_str())
        .map(|e| match e {
            "rs" => "rust",
            "py" => "python",
            "js" | "jsx" => "javascript",
            "go" => "go",
            _ => "unknown",
        })
        .unwrap_or("unknown")
}

// INTENT: Collect explicit import sources from Import-kind symbols, sorted and ready for merging.
fn collect_explicit_imports(ids: &[SymbolId], registry: &SymbolRegistry) -> Vec<String> {
    let mut explicit: Vec<String> = ids
        .iter()
        .filter(|id| {
            registry
                .get(id)
                .map(|e| e.kind == SymbolKind::Import)
                .unwrap_or(false)
        })
        .filter_map(|id| registry.get(id))
        .map(|e| e.source.clone())
        .collect();
    explicit.sort();
    explicit
}

// INTENT: Render a single file's content from ordered entries and merged imports.
fn render_file(entries: &[&SymbolEntry], all_imports: &[String], _max_lines: usize) -> String {
    let mut content = String::new();
    content.push_str("// Generated by trusty-agents symbol emitter — do not edit directly\n\n");

    if !all_imports.is_empty() {
        for imp in all_imports {
            content.push_str(imp);
            content.push('\n');
        }
        content.push('\n');
    }

    for entry in entries {
        content.push_str(&entry.source);
        content.push_str("\n\n");
    }

    content
}

// INTENT: Project the registry to a deterministic path→source map, delegating partition/order to the strategy.
pub fn emit(
    registry: &SymbolRegistry,
    rules: &LayoutRules,
    strategy: &dyn EmitStrategy,
) -> Result<HashMap<PathBuf, String>> {
    let file_symbols = strategy
        .partition(registry, rules)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut sorted_files: Vec<PathBuf> = file_symbols.keys().cloned().collect();
    sorted_files.sort();

    let mut outputs: HashMap<PathBuf, String> = HashMap::new();

    for file in sorted_files {
        let ids = &file_symbols[&file];
        let lang = detect_language(&file);

        let ordered_ids = strategy
            .order_within_file(ids, registry)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let entries: Vec<&SymbolEntry> = ordered_ids
            .iter()
            .filter_map(|id| registry.get(id))
            .collect();

        let mut auto_imports = generate_imports(&entries, lang);
        let explicit_imports = collect_explicit_imports(ids, registry);
        auto_imports.extend(explicit_imports);
        auto_imports.sort();
        auto_imports.dedup();

        let content = render_file(&entries, &auto_imports, rules.max_file_lines);
        outputs.insert(file, content);
    }

    Ok(outputs)
}

// INTENT: Write emitted files to disk in sorted order, creating directories as needed.
pub fn apply_emit(outputs: &HashMap<PathBuf, String>, output_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let mut sorted_paths: Vec<&PathBuf> = outputs.keys().collect();
    sorted_paths.sort();

    for rel_path in sorted_paths {
        let abs_path = output_dir.join(rel_path);
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs_path, outputs[rel_path].as_bytes())?;
        written.push(abs_path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symgraph::strategy::ModulePathStrategy;

    #[test]
    fn test_assign_file_with_module() {
        let id = crate::symgraph::registry::SymbolId::new("api::handlers", "process");
        let path = assign_file(&id, "src");
        assert_eq!(path, std::path::PathBuf::from("src/api/handlers.rs"));
    }

    #[test]
    fn test_assign_file_root() {
        let id = crate::symgraph::registry::SymbolId::new("", "main");
        let path = assign_file(&id, "src");
        assert_eq!(path, std::path::PathBuf::from("src/main.rs"));
    }

    #[test]
    fn test_emit_is_deterministic() {
        let mut reg =
            crate::symgraph::registry::SymbolRegistry::new(std::path::PathBuf::from("/tmp"));
        reg.insert(crate::symgraph::registry::SymbolEntry::new(
            crate::symgraph::registry::SymbolId::new("utils", "helper"),
            crate::symgraph::registry::SymbolKind::Function,
            "fn helper() {}".into(),
            "rust",
        ));
        let rules = LayoutRules::default();
        let strategy = ModulePathStrategy::default();
        let out1 = emit(&reg, &rules, &strategy).unwrap();
        let out2 = emit(&reg, &rules, &strategy).unwrap();
        assert_eq!(out1, out2);
    }
}
