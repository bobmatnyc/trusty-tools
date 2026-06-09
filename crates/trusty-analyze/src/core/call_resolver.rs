//! Post-merge call-edge resolution pass.
//!
//! Why: Tree-sitter adapters emit call-edge targets in an unresolved sentinel
//! form (`{lang}:{kind}::{callee}`) because they have no cross-file symbol
//! resolution.  After the linker deduplicates nodes we have the full canonical
//! node set and can do a best-effort name match to replace sentinel targets
//! with real node ids, making the call graph traversable.
//!
//! What: Builds a `(language, bare_name) → Vec<&KgNode>` index over the
//! canonical node set, then rewrites each Calls edge whose target is an
//! unresolved sentinel.  Same-file candidates are preferred; genuinely
//! unresolvable (external) targets are left as-is.
//!
//! Test: `resolve_calls_*` tests below cover intra-file, cross-file,
//! class-method-qualified, external, and high-resolution-rate scenarios.

use std::collections::HashMap;

use crate::lang::call_target::parse_call_target;
use crate::types::{KgEdgeKind, KgGraph, KgNode};

/// Resolve unresolved call-edge targets to canonical node ids.
///
/// Why: Adapters emit call edges with sentinel targets of the form
/// `{lang}:{kind}::{callee}` because tree-sitter has no cross-file symbol
/// resolution.  After deduplication we have the full canonical node set, so
/// we do a best-effort name match here: same-file nodes are preferred, then
/// any unique match across the whole graph.  Targets that cannot be matched
/// are left as-is (they represent external or unresolvable calls and are
/// intentionally kept so consumers can distinguish them from real nodes).
///
/// What: Builds a lookup index `(lang, name) → Vec<&KgNode>` from the
/// canonical node set, then for every Calls edge whose `to` matches the
/// unresolved sentinel format, replaces `to` with the best-matching node id.
///
/// Test: See module-level tests below.
pub fn resolve_calls(graph: KgGraph) -> KgGraph {
    let KgGraph { nodes, mut edges } = graph;

    // Build a lookup: (language, bare_name) → list of candidate node ids.
    // We index on `node.name` (the bare unqualified name) so that even
    // class-qualified nodes like `csharp:Method:Foo.cs:MyClass:Save` are
    // findable under just `"Save"`.
    let mut name_index: HashMap<(&str, &str), Vec<&KgNode>> = HashMap::new();
    for node in &nodes {
        name_index
            .entry((node.language.as_str(), node.name.as_str()))
            .or_default()
            .push(node);
    }

    for edge in edges.iter_mut() {
        if !matches!(edge.kind, KgEdgeKind::Calls) {
            continue;
        }
        let Some(unresolved) = parse_call_target(&edge.to) else {
            // Already a real node id or unsupported format — leave it.
            continue;
        };

        // Determine caller's file from the `from` id for same-file preference.
        // Node ids have the shape `{lang}:{kind}:{file}:{name}`, so the third
        // colon-delimited component is the file.
        let caller_file: Option<&str> = {
            let parts: Vec<&str> = edge.from.splitn(4, ':').collect();
            if parts.len() >= 3 {
                Some(parts[2])
            } else {
                None
            }
        };

        let candidates = name_index
            .get(&(unresolved.lang, unresolved.callee))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        let resolved = match candidates {
            [] => None,
            [single] => Some(single.id.as_str()),
            many => {
                // Multiple nodes share this name.  Prefer same-file first.
                caller_file.and_then(|file| {
                    // Ambiguous cross-file: leave unresolved rather than guess.
                    many.iter().find(|n| n.file == file).map(|n| n.id.as_str())
                })
            }
        };

        if let Some(id) = resolved {
            edge.to = id.to_string();
        }
        // If unresolvable, the sentinel target is preserved as-is.
    }

    // Drop self-loops introduced by resolution.
    edges.retain(|e| e.from != e.to);

    KgGraph { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::call_target::build_call_target;
    use crate::types::{KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};

    fn mk(id: &str, name: &str, kind: KgNodeKind, lang: &str, file: &str) -> KgNode {
        KgNode {
            id: id.into(),
            kind,
            name: name.into(),
            qualified_name: name.into(),
            language: lang.into(),
            file: file.into(),
            start_line: 1,
            end_line: 10,
            doc_comment: None,
            is_public: true,
            extra: serde_json::Value::Null,
        }
    }

    fn cedge(from: &str, to: &str) -> KgEdge {
        KgEdge {
            from: from.into(),
            to: to.into(),
            kind: KgEdgeKind::Calls,
            weight: 1.0,
        }
    }

    /// Core invariant: the name component in node-id equals callee in call target.
    /// This is what #913 broke — the two id schemes were using different components.
    #[test]
    fn node_id_and_target_share_name_component() {
        let node_id = "rust:Function:src/foo.rs:helper";
        let target = build_call_target("rust", "Function", "helper");
        // splitn(4, ':').nth(3) gives the name component of each format.
        assert_eq!(
            node_id.splitn(4, ':').nth(3),
            target.splitn(4, ':').nth(3),
            "name component must match between node id and call target"
        );
    }

    /// Intra-file call resolves to the callee node id.
    #[test]
    fn resolve_calls_intra_file() {
        let caller_id = "rust:Function:foo.rs:caller";
        let helper_id = "rust:Function:foo.rs:helper";
        let mut g = KgGraph::default();
        g.nodes.push(mk(
            caller_id,
            "caller",
            KgNodeKind::Function,
            "rust",
            "foo.rs",
        ));
        g.nodes.push(mk(
            helper_id,
            "helper",
            KgNodeKind::Function,
            "rust",
            "foo.rs",
        ));
        g.edges.push(cedge(
            caller_id,
            &build_call_target("rust", "Function", "helper"),
        ));
        let out = resolve_calls(g);
        let call = out
            .edges
            .iter()
            .find(|e| matches!(e.kind, KgEdgeKind::Calls))
            .unwrap();
        assert_eq!(call.to, helper_id, "intra-file call must resolve");
    }

    /// Cross-file call resolves when callee name is unique across the graph.
    #[test]
    fn resolve_calls_cross_file() {
        let caller_id = "rust:Function:src/a.rs:do_work";
        let callee_id = "rust:Function:src/b.rs:utility";
        let mut g = KgGraph::default();
        g.nodes.push(mk(
            caller_id,
            "do_work",
            KgNodeKind::Function,
            "rust",
            "src/a.rs",
        ));
        g.nodes.push(mk(
            callee_id,
            "utility",
            KgNodeKind::Function,
            "rust",
            "src/b.rs",
        ));
        g.edges.push(cedge(
            caller_id,
            &build_call_target("rust", "Function", "utility"),
        ));
        let out = resolve_calls(g);
        let call = out
            .edges
            .iter()
            .find(|e| matches!(e.kind, KgEdgeKind::Calls))
            .unwrap();
        assert_eq!(call.to, callee_id, "cross-file unique callee must resolve");
    }

    /// Class-qualified C# method call resolves via bare name lookup.
    /// This is the class-qualifier mismatch case from #913.
    #[test]
    fn resolve_calls_class_method_qualified() {
        let caller_id = "csharp:Method:OrderService.cs:OrderService:Process";
        let callee_id = "csharp:Method:Repository.cs:Repository:Save";
        let mut g = KgGraph::default();
        g.nodes.push(mk(
            caller_id,
            "Process",
            KgNodeKind::Method,
            "csharp",
            "OrderService.cs",
        ));
        // "Save" is the bare name; the node id includes class qualifier.
        g.nodes.push(mk(
            callee_id,
            "Save",
            KgNodeKind::Method,
            "csharp",
            "Repository.cs",
        ));
        g.edges.push(cedge(
            caller_id,
            &build_call_target("csharp", "Method", "Save"),
        ));
        let out = resolve_calls(g);
        let call = out
            .edges
            .iter()
            .find(|e| matches!(e.kind, KgEdgeKind::Calls))
            .unwrap();
        assert_eq!(
            call.to, callee_id,
            "class-method call must resolve to qualified node id"
        );
    }

    /// External (stdlib) call preserves the sentinel target.
    #[test]
    fn resolve_calls_external_stays_unresolved() {
        let caller_id = "rust:Function:src/main.rs:main";
        let ext = build_call_target("rust", "Function", "println");
        let mut g = KgGraph::default();
        g.nodes.push(mk(
            caller_id,
            "main",
            KgNodeKind::Function,
            "rust",
            "src/main.rs",
        ));
        g.edges.push(cedge(caller_id, &ext));
        let out = resolve_calls(g);
        let call = out
            .edges
            .iter()
            .find(|e| matches!(e.kind, KgEdgeKind::Calls))
            .unwrap();
        assert_eq!(call.to, ext, "external call must keep sentinel target");
    }

    /// Multi-file fixture asserting ≥75% resolution (was ~5% before fix #913).
    #[test]
    fn resolve_calls_high_resolution_rate() {
        let alpha_id = "rust:Function:a.rs:alpha";
        let beta_id = "rust:Function:a.rs:beta";
        let gamma_id = "rust:Function:b.rs:gamma";
        let mut g = KgGraph::default();
        g.nodes
            .push(mk(alpha_id, "alpha", KgNodeKind::Function, "rust", "a.rs"));
        g.nodes
            .push(mk(beta_id, "beta", KgNodeKind::Function, "rust", "a.rs"));
        g.nodes
            .push(mk(gamma_id, "gamma", KgNodeKind::Function, "rust", "b.rs"));
        // alpha→beta (intra), alpha→gamma (cross), alpha→delta (external), beta→gamma (cross)
        for (from, to) in [
            (alpha_id, "beta"),
            (alpha_id, "gamma"),
            (alpha_id, "delta"),
            (beta_id, "gamma"),
        ] {
            g.edges
                .push(cedge(from, &build_call_target("rust", "Function", to)));
        }
        let out = resolve_calls(g);
        let calls: Vec<_> = out
            .edges
            .iter()
            .filter(|e| matches!(e.kind, KgEdgeKind::Calls))
            .collect();
        let ids: std::collections::HashSet<_> = out.nodes.iter().map(|n| n.id.as_str()).collect();
        let resolved = calls.iter().filter(|e| ids.contains(e.to.as_str())).count();
        assert_eq!(calls.len(), 4, "should have 4 call edges");
        assert_eq!(resolved, 3, "3/4 calls must resolve (delta is external)");
    }
}
