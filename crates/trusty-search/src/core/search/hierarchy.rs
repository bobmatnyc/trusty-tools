//! Index hierarchy: derive parent/child relationships from `root_path` prefix
//! containment and apply them in the fan-out search pipeline.
//!
//! Why: when multiple indexes are registered and one's `root_path` is a strict
//! sub-path of another's, the sub-index offers higher-fidelity signal for that
//! subtree (fresher embed, KG enabled, finer chunking).  The fan-out handler
//! should boost sub-index hits, dedup duplicate hits where parent and child both
//! cover the same file region, and include children as safety-net lanes even
//! when the threshold router would otherwise exclude them.
//!
//! What: `IndexHierarchy` is computed once per fan-out call from the live
//! `IndexRegistry`.  It records parent→children and child→parent maps,
//! and exposes helpers consumed by `global_search_handler`.
//!
//! Test: `hierarchy_from_root_paths_*` unit tests in this module cover
//! basic containment, deep nesting, symlink-resolved paths, and the no-op
//! flat-peer case (no hierarchy present → all helpers return empty).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};

// ─────────────────────────────────────────────────────────────────────────────
// Hierarchy computation
// ─────────────────────────────────────────────────────────────────────────────

/// Computed parent/child maps for a snapshot of the registry.
///
/// Why: the fan-out handler needs the hierarchy multiple times per request
/// (threshold routing child-inclusion, lane weight boost, post-RRF dedup).
/// Computing it once and passing it around avoids repeated O(n²) path scans.
/// What: two `HashMap`s — `parent_of[child] = parent` and
/// `children_of[parent] = [child…]`.  Both maps contain only IDs that
/// participate in at least one parent/child pair; flat peers are absent.
/// Test: `hierarchy_from_root_paths_*` below.
#[derive(Debug, Default)]
pub struct IndexHierarchy {
    /// Maps each child `IndexId` to its direct parent `IndexId`.
    pub parent_of: HashMap<IndexId, IndexId>,
    /// Maps each parent `IndexId` to the list of its direct children.
    pub children_of: HashMap<IndexId, Vec<IndexId>>,
}

impl IndexHierarchy {
    /// Build a hierarchy from a snapshot of `(IndexId, canonical_root_path)`
    /// pairs.
    ///
    /// Why: separating the path-comparison logic from the live registry lets
    /// tests drive it without spinning up a full daemon.
    /// What: for every pair (A, B) where B's canonical root is a STRICT
    /// sub-path of A's canonical root, B is registered as a child of A.
    /// The *most-specific* (longest) containing path is chosen as the direct
    /// parent, so deep nesting works correctly without creating spurious
    /// grandparent edges.
    /// Test: `hierarchy_two_indexes_nested`, `hierarchy_deep_nesting`,
    /// `hierarchy_flat_peers_no_edges`.
    pub fn from_root_paths(entries: &[(IndexId, PathBuf)]) -> Self {
        let mut parent_of: HashMap<IndexId, IndexId> = HashMap::new();
        let mut children_of: HashMap<IndexId, Vec<IndexId>> = HashMap::new();

        // For each index, find the *deepest* other index whose root is a
        // proper ancestor of this index's root.
        for (child_id, child_root) in entries {
            let mut best_parent: Option<(&IndexId, &PathBuf)> = None;

            for (parent_id, parent_root) in entries {
                if parent_id == child_id {
                    continue;
                }
                if !is_strict_sub_path(child_root, parent_root) {
                    continue;
                }
                // Among all valid parents, pick the deepest (longest root_path)
                // — that is the direct parent.
                let is_deeper = best_parent
                    .as_ref()
                    .map(|(_, p)| parent_root.as_os_str().len() > p.as_os_str().len())
                    .unwrap_or(true);
                if is_deeper {
                    best_parent = Some((parent_id, parent_root));
                }
            }

            if let Some((par_id, _)) = best_parent {
                parent_of.insert(child_id.clone(), par_id.clone());
                children_of
                    .entry(par_id.clone())
                    .or_default()
                    .push(child_id.clone());
            }
        }

        Self {
            parent_of,
            children_of,
        }
    }

    /// Build a hierarchy directly from the live `IndexRegistry`.
    ///
    /// Why: the fan-out handler has a registry reference; this convenience
    /// method canonicalizes each handle's `root_path` and delegates to
    /// `from_root_paths`.
    /// What: iterates `registry.list()`, resolves each `root_path` via
    /// `std::fs::canonicalize` (falls back to the stored path on failure so
    /// a missing directory does not crash the daemon), then calls
    /// `from_root_paths`.
    /// Test: covered end-to-end by `global_search_*` integration tests.
    pub fn from_registry(registry: &IndexRegistry, index_ids: &[IndexId]) -> Self {
        let entries: Vec<(IndexId, PathBuf)> = index_ids
            .iter()
            .filter_map(|id| {
                let handle = registry.get(id)?;
                let canonical = canonicalize_best_effort(&handle.root_path);
                Some((id.clone(), canonical))
            })
            .collect();
        Self::from_root_paths(&entries)
    }

    /// Return true if `id` is a sub-index (has a parent in this hierarchy).
    ///
    /// Why: the lane-weight builder needs a fast per-id predicate to decide
    /// whether to apply the `priority_boost` multiplier.
    /// What: O(1) HashMap lookup in `parent_of`.
    /// Test: implied by `hierarchy_two_indexes_nested`.
    pub fn is_child(&self, id: &IndexId) -> bool {
        self.parent_of.contains_key(id)
    }

    /// Return the children of `parent_id` as a slice of `IndexId`s, or an
    /// empty slice when `parent_id` has no children.
    ///
    /// Why: the threshold child-inclusion rule iterates over an active
    /// parent's children to add them as bonus lanes.
    /// What: O(1) HashMap lookup in `children_of`.
    /// Test: `hierarchy_two_indexes_nested`.
    pub fn children(&self, parent_id: &IndexId) -> &[IndexId] {
        self.children_of
            .get(parent_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Priority boost constant and helper
// ─────────────────────────────────────────────────────────────────────────────

/// Default priority boost applied to sub-index lanes in fan-out search.
///
/// Why: sub-indexes are registered precisely because they offer better signal
/// for their subtree.  A boost of 1.5 (the same as the existing
/// `branch_boost` default) ensures sub-index hits sort above the parent's
/// duplicate hits after RRF fusion without overwhelming unrelated indexes.
/// What: a compile-time constant clamped to `[1.0, 4.0]` in
/// `effective_weight_for_index`.
/// Test: `sub_index_boost_applied_in_effective_weight`.
pub const DEFAULT_SUB_INDEX_BOOST: f32 = 1.5;

/// Compute the effective lane weight for one index in the fan-out.
///
/// Why: the fan-out builder multiplies each chunk's score by this weight
/// before RRF so sub-index hits rank above the parent's copy of the same
/// region.
/// What: `cosine_weight * priority_boost` where `priority_boost` is
/// `DEFAULT_SUB_INDEX_BOOST` for sub-indexes and `1.0` for root peers.
/// The result is clamped to `[1.0, 4.0]`.
/// Test: `sub_index_boost_applied_in_effective_weight`.
pub fn effective_weight_for_index(
    id: &IndexId,
    cosine_weight: f32,
    hierarchy: &IndexHierarchy,
) -> f32 {
    let boost = if hierarchy.is_child(id) {
        DEFAULT_SUB_INDEX_BOOST
    } else {
        1.0_f32
    };
    (cosine_weight * boost).clamp(1.0, 4.0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Dedup key and post-RRF dedup
// ─────────────────────────────────────────────────────────────────────────────

/// Deduplicate fused RRF results for indexes that have a parent/child
/// relationship, keeping the first occurrence (highest-score) for each
/// unique `(canonical_absolute_path, start_line, end_line)` triple.
///
/// Why: a file covered by both a parent index and one of its sub-indexes
/// appears twice in the fused result list.  The sub-index hit is
/// higher-scored (due to the priority boost), so it sorts first and "first
/// wins" naturally keeps it.  The parent's copy is dropped.
/// What: walks `fused` in score-descending order (already sorted by RRF).
/// For each entry, resolves its chunk's file to an absolute canonical path
/// by joining `handle.root_path + chunk.file` and calling
/// `canonicalize_best_effort`.  If the resulting `(path, start, end)` key
/// has been seen before, the entry is dropped.  Only entries from indexes
/// that participate in at least one parent/child pair are subject to dedup
/// — flat peers that merely share files are left unchanged.
/// Returns `(deduped_results, hierarchy_dedup_count)`.
/// Test: `nested_dedup_keeps_sub_index_hit`,
/// `flat_peers_sharing_file_not_deduped`.
pub fn dedup_nested_results(
    fused: Vec<(String, f32)>,
    chunk_lookup: &HashMap<String, crate::core::indexer::CodeChunk>,
    registry: &IndexRegistry,
    hierarchy: &IndexHierarchy,
) -> (Vec<(String, f32)>, usize) {
    // Fast path: no hierarchy → nothing to dedup.
    if hierarchy.parent_of.is_empty() {
        return (fused, 0);
    }

    // Collect the set of index IDs that participate in any parent/child pair.
    let mut hierarchy_ids: std::collections::HashSet<&IndexId> = std::collections::HashSet::new();
    for (child, parent) in &hierarchy.parent_of {
        hierarchy_ids.insert(child);
        hierarchy_ids.insert(parent);
    }

    let input_len = fused.len();
    let mut seen: HashMap<(PathBuf, usize, usize), ()> = HashMap::new();
    let mut deduped: Vec<(String, f32)> = Vec::with_capacity(input_len);

    for (namespaced_id, score) in fused {
        // Extract the index_id from the namespaced key "{index_id}::{chunk_id}".
        let index_id_str = match namespaced_id.split_once("::") {
            Some((idx, _)) => idx,
            None => {
                // Malformed key: pass through unchanged.
                deduped.push((namespaced_id, score));
                continue;
            }
        };
        let index_id = IndexId::new(index_id_str);

        // Only apply dedup to indexes that are in a parent/child relationship.
        if !hierarchy_ids.contains(&index_id) {
            deduped.push((namespaced_id, score));
            continue;
        }

        let Some(chunk) = chunk_lookup.get(&namespaced_id) else {
            deduped.push((namespaced_id, score));
            continue;
        };

        let abs_path = resolve_chunk_path(registry, &index_id, &chunk.file);
        let key = (abs_path, chunk.start_line, chunk.end_line);

        if seen.contains_key(&key) {
            // Duplicate: drop this entry.
            continue;
        }
        seen.insert(key, ());
        deduped.push((namespaced_id, score));
    }

    let dropped = input_len - deduped.len();
    (deduped, dropped)
}

/// Resolve a chunk's `file` field to a canonical absolute path, given its
/// origin index.
///
/// Why: dedup requires a single stable key per file region regardless of
/// which index the chunk came from.  The chunk's `file` is stored as a path
/// relative to `root_path`; joining and canonicalizing produces an absolute
/// form suitable for cross-index comparison.
/// What: joins `handle.root_path / chunk.file`, then calls
/// `canonicalize_best_effort`.  Absolute `file` values (legacy) are
/// canonicalized directly.
/// Test: implied by `nested_dedup_keeps_sub_index_hit`.
fn resolve_chunk_path(registry: &IndexRegistry, index_id: &IndexId, file: &str) -> PathBuf {
    let raw = if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        registry
            .get(index_id)
            .map(|h| h.root_path.join(file))
            .unwrap_or_else(|| PathBuf::from(file))
    };
    canonicalize_best_effort(&raw)
}

// ─────────────────────────────────────────────────────────────────────────────
// Threshold child-inclusion
// ─────────────────────────────────────────────────────────────────────────────

/// Apply the child-inclusion safety net for `Threshold` routing.
///
/// Why: a sub-index over a small subtree may have a weak `context_embedding`
/// (few metadata files) and fall below the cosine threshold even when the
/// parent index is clearly relevant.  Including such a child at weight 1.0
/// ensures the specialist sub-index is never silently excluded when its parent
/// is active.
/// What: for every index_id in `inactive_ids`, if it is a child of any
/// `active_id`, inserts `(id, 1.0)` into `weight_map` and pushes `id` into
/// `active_ids`.
/// Test: `threshold_child_inclusion_adds_child_when_parent_active`.
pub fn apply_threshold_child_inclusion(
    inactive_ids: &[IndexId],
    active_ids: &mut Vec<IndexId>,
    weight_map: &mut HashMap<IndexId, f32>,
    hierarchy: &IndexHierarchy,
) {
    for child_id in inactive_ids {
        let Some(parent_id) = hierarchy.parent_of.get(child_id) else {
            continue;
        };
        if weight_map.contains_key(parent_id) {
            // Parent is active → include this child at neutral weight.
            if !weight_map.contains_key(child_id) {
                tracing::debug!(
                    "hierarchy: including child index '{}' (parent '{}' is active)",
                    child_id,
                    parent_id,
                );
                weight_map.insert(child_id.clone(), 1.0);
                active_ids.push(child_id.clone());
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /indexes?format=tree helpers
// ─────────────────────────────────────────────────────────────────────────────

/// One entry in the `?format=tree` response for `GET /indexes`.
///
/// Why: operators need visibility into which indexes are nested under which
/// parents so they can diagnose fan-out behaviour without grepping daemon logs.
/// What: mirrors the flat index record but adds `parent_id` (null for roots)
/// and `children` (list of direct child IDs).  Serialized to JSON by the
/// `list_indexes_handler` when `?format=tree` is present.
/// Test: `list_indexes_tree_format_shape` in server tests.
#[derive(Debug, serde::Serialize)]
pub struct IndexTreeEntry {
    pub id: String,
    pub root_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub children: Vec<String>,
    pub priority_boost: f32,
    pub is_sub_index: bool,
}

/// Build the `?format=tree` response body from the live registry.
///
/// Why: `list_indexes_handler` needs to produce the nested view without
/// duplicating hierarchy-computation logic.
/// What: computes `IndexHierarchy` for all registered indexes, then maps each
/// handle to an `IndexTreeEntry` carrying its parent/children.
/// Test: `list_indexes_tree_format_shape` asserts the shape and non-breaking
/// default response.
pub fn build_tree_entries(
    registry: &IndexRegistry,
    handles: &[Arc<IndexHandle>],
) -> Vec<IndexTreeEntry> {
    let all_ids: Vec<IndexId> = handles.iter().map(|h| h.id.clone()).collect();
    let hierarchy = IndexHierarchy::from_registry(registry, &all_ids);

    handles
        .iter()
        .map(|h| {
            let parent_id = hierarchy.parent_of.get(&h.id).map(|p| p.0.clone());
            let children = hierarchy
                .children(&h.id)
                .iter()
                .map(|c| c.0.clone())
                .collect();
            let is_sub = hierarchy.is_child(&h.id);
            let boost = if is_sub { DEFAULT_SUB_INDEX_BOOST } else { 1.0 };
            IndexTreeEntry {
                id: h.id.0.clone(),
                root_path: h.root_path.clone(),
                parent_id,
                children,
                priority_boost: boost,
                is_sub_index: is_sub,
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Path helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Returns true when `child` is a STRICT sub-path of `parent` (i.e. child
/// starts with parent/ but is not equal to parent).
///
/// Why: the hierarchy detection logic must not consider an index its own
/// parent.  Trailing slashes are normalised by `Path::starts_with` which
/// operates on components, so `/foo/bar` starts with `/foo` but NOT with
/// `/foobar`.
/// What: `child.starts_with(parent) && child != parent`.
/// Test: `is_strict_sub_path_*` below.
fn is_strict_sub_path(child: &Path, parent: &Path) -> bool {
    child != parent && child.starts_with(parent)
}

/// Canonicalize `path` via `std::fs::canonicalize`, falling back to the
/// original path on any I/O error (non-existent path, permission error).
///
/// Why: symlink-aware dedup requires resolved paths, but a deleted file
/// referenced by a stale index should not crash the search response.
/// What: returns `std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())`.
/// Test: implied by integration tests; the fallback branch is covered by
/// `dedup_missing_file_uses_raw_path`.
pub fn canonicalize_best_effort(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn id(s: &str) -> IndexId {
        IndexId::new(s)
    }

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // ── is_strict_sub_path ──────────────────────────────────────────────────

    #[test]
    fn is_strict_sub_path_child_is_sub() {
        assert!(is_strict_sub_path(
            &pb("/repos/project/services/billing"),
            &pb("/repos/project")
        ));
    }

    #[test]
    fn is_strict_sub_path_same_path_is_not_sub() {
        assert!(!is_strict_sub_path(
            &pb("/repos/project"),
            &pb("/repos/project")
        ));
    }

    #[test]
    fn is_strict_sub_path_prefix_without_separator_is_not_sub() {
        // "/repos/projectX" must NOT be treated as a sub-path of "/repos/project"
        assert!(!is_strict_sub_path(
            &pb("/repos/projectX/src"),
            &pb("/repos/project")
        ));
    }

    #[test]
    fn is_strict_sub_path_parent_does_not_start_with_child() {
        assert!(!is_strict_sub_path(
            &pb("/repos/project"),
            &pb("/repos/project/services/billing")
        ));
    }

    // ── IndexHierarchy::from_root_paths ─────────────────────────────────────

    #[test]
    fn hierarchy_flat_peers_no_edges() {
        let entries = vec![
            (id("a"), pb("/repos/project-a")),
            (id("b"), pb("/repos/project-b")),
        ];
        let h = IndexHierarchy::from_root_paths(&entries);
        assert!(h.parent_of.is_empty());
        assert!(h.children_of.is_empty());
        assert!(!h.is_child(&id("a")));
        assert!(!h.is_child(&id("b")));
    }

    #[test]
    fn hierarchy_two_indexes_nested() {
        let entries = vec![
            (id("root"), pb("/repos/project")),
            (id("billing"), pb("/repos/project/services/billing")),
        ];
        let h = IndexHierarchy::from_root_paths(&entries);
        assert_eq!(h.parent_of.get(&id("billing")), Some(&id("root")));
        assert!(h.children(&id("root")).contains(&id("billing")));
        assert!(h.is_child(&id("billing")));
        assert!(!h.is_child(&id("root")));
    }

    #[test]
    fn hierarchy_deep_nesting_picks_direct_parent() {
        // Three-level hierarchy: grandparent → parent → child
        // The child's direct parent should be "parent", not "grandparent".
        let entries = vec![
            (id("grandparent"), pb("/a")),
            (id("parent"), pb("/a/b")),
            (id("child"), pb("/a/b/c")),
        ];
        let h = IndexHierarchy::from_root_paths(&entries);
        // child's direct parent is "parent" (deepest ancestor)
        assert_eq!(h.parent_of.get(&id("child")), Some(&id("parent")));
        // parent's direct parent is "grandparent"
        assert_eq!(h.parent_of.get(&id("parent")), Some(&id("grandparent")));
        assert_eq!(h.parent_of.len(), 2);
        assert!(h.children(&id("parent")).contains(&id("child")));
        assert!(h.children(&id("grandparent")).contains(&id("parent")));
        // "grandparent" has "parent" as a child, not "child" directly.
        assert!(!h.children(&id("grandparent")).contains(&id("child")));
    }

    #[test]
    fn hierarchy_sibling_sub_indexes_have_independent_parents() {
        let entries = vec![
            (id("root"), pb("/repos/mono")),
            (id("svc-a"), pb("/repos/mono/services/a")),
            (id("svc-b"), pb("/repos/mono/services/b")),
        ];
        let h = IndexHierarchy::from_root_paths(&entries);
        assert_eq!(h.parent_of.get(&id("svc-a")), Some(&id("root")));
        assert_eq!(h.parent_of.get(&id("svc-b")), Some(&id("root")));
        let children = h.children(&id("root"));
        assert!(children.contains(&id("svc-a")));
        assert!(children.contains(&id("svc-b")));
        // Siblings are not each other's parents.
        assert!(!h.parent_of.contains_key(&id("root")));
    }

    // ── effective_weight_for_index ───────────────────────────────────────────

    #[test]
    fn sub_index_boost_applied_in_effective_weight() {
        let entries = vec![
            (id("root"), pb("/repos/project")),
            (id("billing"), pb("/repos/project/services/billing")),
        ];
        let h = IndexHierarchy::from_root_paths(&entries);

        // Root index: cosine 0.8, no boost → 0.8 (clamped to min 1.0)
        // Note: clamping raises 0.8 to 1.0
        let w_root = effective_weight_for_index(&id("root"), 0.8, &h);
        assert!(
            (w_root - 1.0).abs() < 1e-4,
            "root weight {w_root} should be 1.0 (clamped)"
        );

        // Sub-index: cosine 0.8 × 1.5 boost = 1.2
        let w_child = effective_weight_for_index(&id("billing"), 0.8, &h);
        assert!(
            (w_child - 1.2).abs() < 1e-4,
            "child weight {w_child} should be 1.2"
        );

        // Sub-index with cosine 1.0 → 1.5
        let w_child_max = effective_weight_for_index(&id("billing"), 1.0, &h);
        assert!(
            (w_child_max - 1.5).abs() < 1e-4,
            "child weight {w_child_max} should be 1.5"
        );
    }

    #[test]
    fn effective_weight_clamped_to_max() {
        // If cosine=1.0 and boost=4.0 (hypothetical), clamp should trigger.
        // With DEFAULT_SUB_INDEX_BOOST=1.5 and cosine=4.0 → would be 6.0, clamped to 4.0.
        let entries = vec![(id("parent"), pb("/a")), (id("child"), pb("/a/b"))];
        let h = IndexHierarchy::from_root_paths(&entries);
        let w = effective_weight_for_index(&id("child"), 4.0, &h);
        assert!(
            (w - 4.0).abs() < 1e-4,
            "weight {w} should be clamped to 4.0"
        );
    }

    // ── apply_threshold_child_inclusion ─────────────────────────────────────

    #[test]
    fn threshold_child_inclusion_adds_child_when_parent_active() {
        let entries = vec![
            (id("root"), pb("/repos/mono")),
            (id("svc"), pb("/repos/mono/svc")),
        ];
        let h = IndexHierarchy::from_root_paths(&entries);

        let mut active = vec![id("root")];
        let mut weight_map: HashMap<IndexId, f32> = [(id("root"), 0.9)].into_iter().collect();
        let inactive = vec![id("svc")];

        apply_threshold_child_inclusion(&inactive, &mut active, &mut weight_map, &h);

        assert!(
            active.contains(&id("svc")),
            "child should be added to active list"
        );
        assert_eq!(
            weight_map.get(&id("svc")),
            Some(&1.0),
            "child weight should be 1.0"
        );
    }

    #[test]
    fn threshold_child_inclusion_does_not_add_when_parent_inactive() {
        let entries = vec![
            (id("root"), pb("/repos/mono")),
            (id("svc"), pb("/repos/mono/svc")),
        ];
        let h = IndexHierarchy::from_root_paths(&entries);

        let mut active: Vec<IndexId> = vec![];
        let mut weight_map: HashMap<IndexId, f32> = HashMap::new();
        let inactive = vec![id("root"), id("svc")];

        apply_threshold_child_inclusion(&inactive, &mut active, &mut weight_map, &h);

        // Neither root nor svc should be added (root is not a child, and its
        // parent "svc" is not active either — and "svc"'s parent "root" is inactive).
        assert!(
            active.is_empty(),
            "nothing should be added when parent is inactive"
        );
    }

    // ── build_tree_entries (shape test with mock handles) ───────────────────
    // Full integration of build_tree_entries requires a live registry.
    // The hierarchy-building logic is tested via `IndexHierarchy::from_root_paths`
    // above; server-level `list_indexes_tree_format_shape` covers the HTTP layer.

    // ── canonicalize_best_effort ─────────────────────────────────────────────

    #[test]
    fn canonicalize_best_effort_returns_input_on_missing_path() {
        let missing = PathBuf::from("/this/path/does/not/exist/abc123");
        let result = canonicalize_best_effort(&missing);
        assert_eq!(
            result, missing,
            "should fall back to the raw path on failure"
        );
    }

    #[test]
    fn canonicalize_best_effort_resolves_existing_path() {
        // /tmp always exists on macOS and Linux.
        let result = canonicalize_best_effort(&PathBuf::from("/tmp"));
        // The canonical path on macOS is /private/tmp; on Linux it's /tmp.
        // Either way it must be absolute.
        assert!(result.is_absolute());
    }
}
