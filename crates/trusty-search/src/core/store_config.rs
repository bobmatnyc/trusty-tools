//! Out-of-core / memory-footprint configuration knobs for the usearch HNSW
//! vector store (issue #709).
//!
//! Why: the near-term "quick win" memory reductions for the larger-than-RAM
//! vector index are controlled by environment variables. Parsing and validating
//! those env vars is pure, self-contained logic that does not need the
//! `UsearchStore` lock machinery, so it lives in its own focused module with its
//! own unit tests rather than inflating `store.rs` (which is already at its
//! frozen line-cap budget).
//! What: defines [`MmapServeMode`] (the `TRUSTY_HNSW_MMAP_SERVE` read-path knob),
//! with a `from_env()` resolver.
//! Test: see `tests` below — every accepted/rejected env spelling is covered
//! without touching the filesystem or usearch.

/// Environment variable selecting whether warm-booted HNSW snapshots are served
/// directly from the memory-mapped `Index::view` (low RSS) or eagerly promoted
/// to a heap-resident copy on load (higher RSS, no cold page-fault latency).
pub const HNSW_MMAP_SERVE_ENV: &str = "TRUSTY_HNSW_MMAP_SERVE";

/// How a warm-booted (on-disk) HNSW snapshot is served on the read/search path.
///
/// Why (issue #709, quick win #1): the warm-boot memory fix opens snapshots via
/// `Index::view`, which memory-maps the file so the OS page cache — not the heap
/// — holds the HNSW graph. A pure read/search workload then never duplicates the
/// graph onto the heap; promotion to a mutable heap copy happens lazily on the
/// first *write*. That is the right default (much lower resident RSS when a
/// daemon holds hundreds of indexes, most of which are only ever queried). The
/// **trade-off**: the first touch of a cold page faults it in from disk, adding
/// latency to the first few queries after boot. On local SSDs this is
/// negligible; on **EFS / NFS-backed** snapshot storage a fault is a network
/// round-trip and can be materially slower, so operators who prefer to pay the
/// RSS cost up front (and avoid cold-fault tail latency) can opt out, which
/// makes `load_from` eagerly promote the snapshot to a heap copy at load time.
/// What: a two-state enum resolved from [`HNSW_MMAP_SERVE_ENV`]; `Mmap` (default)
/// serves from the view, `EagerHeap` promotes on load.
/// Test: `tests::mmap_serve_mode_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MmapServeMode {
    /// Serve searches directly from the mmap view; promote to heap only on the
    /// first write. Lowest RSS. **Default.**
    #[default]
    Mmap,
    /// Promote the snapshot to a heap-resident mutable copy at load time so all
    /// serving is heap-resident (the pre-memory-fix behaviour). Higher RSS, no
    /// cold page-fault latency on first query.
    EagerHeap,
}

impl MmapServeMode {
    /// Resolve the serve mode from [`HNSW_MMAP_SERVE_ENV`].
    ///
    /// Why: a single place that turns the operator-facing string into the typed
    /// mode, so callers never re-implement the truthiness parsing.
    /// What: unset / `1` / `true` / `yes` / `on` (any case, trimmed) → `Mmap`
    /// (the default, mmap serving enabled); `0` / `false` / `no` / `off` →
    /// `EagerHeap` (opt out). Any other value is treated as the default with a
    /// `tracing::warn!` so a typo never silently flips behaviour.
    /// Test: `tests::mmap_serve_mode_from_env_*`.
    pub fn from_env() -> Self {
        match std::env::var(HNSW_MMAP_SERVE_ENV) {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::default(),
        }
    }

    /// Pure parser split out from [`Self::from_env`] for testability.
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "1" | "true" | "yes" | "on" | "enabled" => Self::Mmap,
            "0" | "false" | "no" | "off" | "disabled" => Self::EagerHeap,
            other => {
                tracing::warn!(
                    "{HNSW_MMAP_SERVE_ENV}={other:?} is not a recognised boolean; \
                     defaulting to mmap-view serving (enabled)"
                );
                Self::default()
            }
        }
    }

    /// `true` when warm-booted snapshots should be promoted to heap at load time.
    pub fn promote_on_load(self) -> bool {
        matches!(self, Self::EagerHeap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmap_serve_mode_default_is_mmap() {
        assert_eq!(MmapServeMode::default(), MmapServeMode::Mmap);
        assert!(!MmapServeMode::default().promote_on_load());
    }

    #[test]
    fn mmap_serve_mode_parse_enabled_spellings() {
        for s in ["", "1", "true", "TRUE", " yes ", "On", "enabled"] {
            assert_eq!(
                MmapServeMode::parse(s),
                MmapServeMode::Mmap,
                "{s:?} should enable mmap serving"
            );
        }
    }

    #[test]
    fn mmap_serve_mode_parse_disabled_spellings() {
        for s in ["0", "false", "FALSE", " no ", "Off", "disabled"] {
            assert_eq!(
                MmapServeMode::parse(s),
                MmapServeMode::EagerHeap,
                "{s:?} should disable mmap serving (eager heap)"
            );
            assert!(MmapServeMode::parse(s).promote_on_load());
        }
    }

    #[test]
    fn mmap_serve_mode_parse_garbage_defaults_to_mmap() {
        assert_eq!(MmapServeMode::parse("banana"), MmapServeMode::Mmap);
    }
}
