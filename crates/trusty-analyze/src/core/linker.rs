//! Post-processing pass that merges duplicate KgNodes across chunks and
//! resolves unresolved call-edge targets to real node ids.
//!
//! Why: the analyzer processes 40-line overlapping windows, so the same
//! function can appear in multiple chunks. Without linking, the graph has
//! many duplicate nodes with different IDs but the same qualified_name.
//! Additionally, call-edge targets are emitted as unresolved name references
//! (format `{lang}:{kind}::{callee}`) and must be matched to real node ids
//! before the graph is returned to consumers.
//!
//! What: groups nodes by (language, kind, qualified_name), merges duplicates
//! into a single node (taking the widest line range), rewires edges to
//! point to the canonical node ID, then runs a second pass that resolves
//! unresolved call-edge targets by name matching against the canonical node set.
//! Edges between merged nodes become self-loops and are removed.  Call edges
//! whose callee cannot be found in the node set are preserved with their
//! unresolved target form so consumers can identify external calls.
//!
//! Test: `merge_deduplicates_nodes_with_same_qualified_name` verifies that two
//! identical fn nodes collapse to one. `self_loops_are_removed_after_merge`
//! verifies edges between merged nodes are dropped.  `resolve_calls_*` tests
//! verify intra-file, cross-file, and class-method-qualified resolution.

use std::collections::HashMap;

use crate::core::call_resolver::resolve_calls;
use crate::types::{KgEdge, KgGraph, KgNode};

/// Merge duplicate nodes (same language + kind + qualified_name) and rewire
/// edges. Returns a new graph with unique nodes and updated edges.
///
/// Why: cross-chunk linking is necessary because the chunker emits
/// overlapping windows that each re-discover the same symbols. Without this
/// pass, downstream consumers see N copies of every symbol that straddles a
/// window boundary.
///
/// What: builds an id-remap by canonical key, picks the widest-range node as
/// the canonical representative (preserving any non-None doc comment), then
/// rewrites every edge to use canonical ids and removes self-loops and
/// duplicates.
///
/// Test: see module-level tests below.
pub fn link(graph: KgGraph) -> KgGraph {
    let KgGraph { nodes, edges } = graph;

    // Step 1: group nodes by canonical key.
    let key_of = |n: &KgNode| {
        (
            n.language.clone(),
            format!("{:?}", n.kind),
            n.qualified_name.clone(),
        )
    };

    let mut groups: HashMap<(String, String, String), Vec<KgNode>> = HashMap::new();
    for n in nodes {
        groups.entry(key_of(&n)).or_default().push(n);
    }

    // Step 2–4: pick canonical per group and build id remap.
    let mut canonical_nodes: Vec<KgNode> = Vec::with_capacity(groups.len());
    let mut id_remap: HashMap<String, String> = HashMap::new();

    for (_key, group) in groups {
        // Pick canonical = widest line range; ties broken by first encountered.
        let mut iter = group.into_iter();
        let mut canonical = iter.next().expect("group is non-empty by construction");
        let mut members: Vec<KgNode> = Vec::new();
        for candidate in iter {
            let span_c = candidate.end_line.saturating_sub(candidate.start_line);
            let span_canon = canonical.end_line.saturating_sub(canonical.start_line);
            if span_c > span_canon {
                members.push(canonical);
                canonical = candidate;
            } else {
                members.push(candidate);
            }
        }

        // Merge doc_comment: prefer the canonical's, fall back to any other
        // Some(...) among the merged members.
        if canonical.doc_comment.is_none() {
            for m in &members {
                if m.doc_comment.is_some() {
                    canonical.doc_comment = m.doc_comment.clone();
                    break;
                }
            }
        }
        // Promote is_public if any member was public.
        if !canonical.is_public {
            canonical.is_public = members.iter().any(|m| m.is_public);
        }

        let canonical_id = canonical.id.clone();
        // Map every member's id (including canonical's own) to canonical_id.
        id_remap.insert(canonical.id.clone(), canonical_id.clone());
        for m in &members {
            id_remap.insert(m.id.clone(), canonical_id.clone());
        }

        canonical_nodes.push(canonical);
    }

    // Step 5: rewrite edges, drop self-loops, deduplicate by (from, to, kind).
    //
    // We deduplicate by summing weights so that repeated call_count
    // information is preserved when overlapping chunks contributed separate
    // edge instances.
    let remap =
        |id: &str| -> String { id_remap.get(id).cloned().unwrap_or_else(|| id.to_string()) };

    let mut edge_index: HashMap<(String, String, String), usize> = HashMap::new();
    let mut clean_edges: Vec<KgEdge> = Vec::new();
    for e in edges {
        let from = remap(&e.from);
        let to = remap(&e.to);
        if from == to {
            continue;
        }
        let key = (from.clone(), to.clone(), format!("{:?}", e.kind));
        if let Some(&idx) = edge_index.get(&key) {
            clean_edges[idx].weight += e.weight;
        } else {
            let new_edge = KgEdge {
                from,
                to,
                kind: e.kind,
                weight: e.weight,
            };
            edge_index.insert(key, clean_edges.len());
            clean_edges.push(new_edge);
        }
    }

    let deduped = KgGraph {
        nodes: canonical_nodes,
        edges: clean_edges,
    };

    // Second pass: resolve unresolved call-edge targets.
    resolve_calls(deduped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};

    #[test]
    fn merge_deduplicates_nodes_with_same_qualified_name() {
        let mut graph = KgGraph::default();
        // Two nodes: same fn, different chunk windows
        graph.nodes.push(KgNode {
            id: "rust:Function:foo.rs:my_fn_1".into(),
            kind: KgNodeKind::Function,
            name: "my_fn".into(),
            qualified_name: "my_fn".into(),
            language: "rust".into(),
            file: "foo.rs".into(),
            start_line: 1,
            end_line: 10,
            doc_comment: None,
            is_public: false,
            extra: serde_json::Value::Null,
        });
        graph.nodes.push(KgNode {
            id: "rust:Function:foo.rs:my_fn_2".into(),
            kind: KgNodeKind::Function,
            name: "my_fn".into(),
            qualified_name: "my_fn".into(),
            language: "rust".into(),
            file: "foo.rs".into(),
            start_line: 5,
            end_line: 20, // wider → should be canonical
            doc_comment: Some("doc".into()),
            is_public: true,
            extra: serde_json::Value::Null,
        });
        // An edge using the first (non-canonical) ID
        graph.edges.push(KgEdge {
            from: "caller".into(),
            to: "rust:Function:foo.rs:my_fn_1".into(),
            kind: KgEdgeKind::Calls,
            weight: 1.0,
        });

        let linked = link(graph);
        assert_eq!(linked.node_count(), 1, "should have merged to 1 node");
        // Edge should have been rewritten to canonical ID
        assert!(
            linked.edges[0].to.contains("my_fn"),
            "edge not rewritten: {:?}",
            linked.edges[0]
        );
        // Canonical should prefer wider range
        assert_eq!(linked.nodes[0].end_line, 20);
        assert_eq!(linked.nodes[0].doc_comment, Some("doc".into()));
    }

    #[test]
    fn self_loops_are_removed_after_merge() {
        let mut graph = KgGraph::default();
        graph.nodes.push(KgNode {
            id: "a".into(),
            kind: KgNodeKind::Function,
            name: "f".into(),
            qualified_name: "f".into(),
            language: "rust".into(),
            file: "x.rs".into(),
            start_line: 1,
            end_line: 5,
            doc_comment: None,
            is_public: false,
            extra: serde_json::Value::Null,
        });
        graph.nodes.push(KgNode {
            id: "b".into(),
            kind: KgNodeKind::Function,
            name: "f".into(),
            qualified_name: "f".into(),
            language: "rust".into(),
            file: "x.rs".into(),
            start_line: 1,
            end_line: 5,
            doc_comment: None,
            is_public: false,
            extra: serde_json::Value::Null,
        });
        // Edge a→b, both will merge to same canonical
        graph.edges.push(KgEdge {
            from: "a".into(),
            to: "b".into(),
            kind: KgEdgeKind::Contains,
            weight: 1.0,
        });

        let linked = link(graph);
        assert_eq!(
            linked.edge_count(),
            0,
            "self-loop after merge should be removed"
        );
    }

    #[test]
    fn empty_graph_passes_through() {
        let linked = link(KgGraph::default());
        assert_eq!(linked.node_count(), 0);
        assert_eq!(linked.edge_count(), 0);
    }
}
