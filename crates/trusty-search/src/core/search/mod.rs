//! Search subsystem: RRF fusion and nested-index hierarchy helpers.
pub mod hierarchy;
pub mod rrf;

pub use hierarchy::{
    apply_threshold_child_inclusion, build_tree_entries, canonicalize_best_effort,
    dedup_nested_results, effective_weight_for_index, IndexHierarchy, IndexTreeEntry,
    DEFAULT_SUB_INDEX_BOOST,
};
pub use rrf::rrf_fuse;
