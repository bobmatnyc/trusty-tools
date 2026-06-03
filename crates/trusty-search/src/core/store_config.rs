//! Out-of-core / memory-footprint configuration knobs for the usearch HNSW
//! vector store (issue #709).
//!
//! Why: the two near-term "quick win" memory reductions for the larger-than-RAM
//! vector index (mmap-view serving and optional vector quantization) are both
//! controlled by environment variables. Parsing and validating those env vars —
//! plus mapping the quantization knob onto usearch's `ScalarKind` — is pure,
//! self-contained logic that does not need the `UsearchStore` lock machinery, so
//! it lives in its own focused module with its own unit tests rather than
//! inflating `store.rs` (which is already at its frozen line-cap budget).
//! What: defines [`MmapServeMode`] (the `TRUSTY_HNSW_MMAP_SERVE` read-path knob)
//! and [`VectorQuant`] (the `TRUSTY_VECTOR_QUANT` create-time knob), each with a
//! `from_env()` resolver, plus the `ScalarKind` mapping.
//! Test: see `tests` below — every accepted/rejected env spelling and the
//! `ScalarKind` mapping are covered without touching the filesystem or usearch.

use usearch::ScalarKind;

/// Environment variable selecting whether warm-booted HNSW snapshots are served
/// directly from the memory-mapped `Index::view` (low RSS) or eagerly promoted
/// to a heap-resident copy on load (higher RSS, no cold page-fault latency).
pub const HNSW_MMAP_SERVE_ENV: &str = "TRUSTY_HNSW_MMAP_SERVE";

/// Environment variable selecting the scalar precision new HNSW indexes are
/// built with. Applied only at index *creation* time; changing it requires a
/// forced reindex (existing snapshots keep the precision they were built with).
pub const VECTOR_QUANT_ENV: &str = "TRUSTY_VECTOR_QUANT";

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

/// Scalar precision a new HNSW index is built with (issue #709, quick win #2).
///
/// Why: usearch can store vectors at reduced precision, trading a small recall
/// loss for a large reduction in resident + on-disk footprint. `F16` halves the
/// per-vector bytes (≈2× smaller), `I8` quarters them (≈4× smaller). Exposing
/// this as a create-time knob lets operators shrink large indexes that don't
/// need full `f32` precision, while the default stays `None` (`f32`) so existing
/// behaviour and recall are unchanged unless explicitly opted in.
/// What: a three-state enum resolved from [`VECTOR_QUANT_ENV`], mapped onto
/// usearch's [`ScalarKind`] via [`Self::scalar_kind`]. The HNSW `search` API
/// still takes `&[f32]` queries regardless of internal precision — usearch
/// quantizes the query internally — so only the index build options change.
/// Test: `tests::vector_quant_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorQuant {
    /// Full 32-bit float precision. **Default** — no recall loss.
    #[default]
    None,
    /// 16-bit half precision — ≈2× smaller vectors, small recall cost.
    F16,
    /// 8-bit integer quantization — ≈4× smaller vectors, larger recall cost.
    I8,
}

impl VectorQuant {
    /// Resolve the quantization kind from [`VECTOR_QUANT_ENV`].
    ///
    /// Why: centralises the operator-facing string → enum mapping so index
    /// creation has a single source of truth.
    /// What: unset / `none` / `f32` → `None`; `f16` / `fp16` / `half` → `F16`;
    /// `i8` / `int8` → `I8` (case-insensitive, trimmed). Any other value falls
    /// back to `None` with a `tracing::warn!`.
    /// Test: `tests::vector_quant_from_env_*`.
    pub fn from_env() -> Self {
        match std::env::var(VECTOR_QUANT_ENV) {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::default(),
        }
    }

    /// Pure parser split out from [`Self::from_env`] for testability.
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "none" | "f32" | "fp32" | "full" => Self::None,
            "f16" | "fp16" | "half" => Self::F16,
            "i8" | "int8" => Self::I8,
            other => {
                tracing::warn!(
                    "{VECTOR_QUANT_ENV}={other:?} is not a recognised quantization kind \
                     (expected none|f16|i8); defaulting to none (f32)"
                );
                Self::default()
            }
        }
    }

    /// Map this knob onto usearch's [`ScalarKind`] for `IndexOptions`.
    ///
    /// Why: the usearch build options take a `ScalarKind`; this is the single
    /// translation point.
    /// What: `None → F32`, `F16 → F16`, `I8 → I8`.
    /// Test: `tests::vector_quant_scalar_kind`.
    pub fn scalar_kind(self) -> ScalarKind {
        match self {
            Self::None => ScalarKind::F32,
            Self::F16 => ScalarKind::F16,
            Self::I8 => ScalarKind::I8,
        }
    }

    /// Human-readable label for startup/diagnostic logging.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "f32 (none)",
            Self::F16 => "f16",
            Self::I8 => "i8",
        }
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

    #[test]
    fn vector_quant_default_is_none() {
        assert_eq!(VectorQuant::default(), VectorQuant::None);
        assert_eq!(VectorQuant::None.scalar_kind(), ScalarKind::F32);
    }

    #[test]
    fn vector_quant_parse_spellings() {
        for s in ["", "none", "f32", "FP32", " full "] {
            assert_eq!(VectorQuant::parse(s), VectorQuant::None, "{s:?}");
        }
        for s in ["f16", "FP16", " half "] {
            assert_eq!(VectorQuant::parse(s), VectorQuant::F16, "{s:?}");
        }
        for s in ["i8", "INT8", " i8 "] {
            assert_eq!(VectorQuant::parse(s), VectorQuant::I8, "{s:?}");
        }
    }

    #[test]
    fn vector_quant_parse_garbage_defaults_to_none() {
        assert_eq!(VectorQuant::parse("bf16"), VectorQuant::None);
        assert_eq!(VectorQuant::parse("q4"), VectorQuant::None);
    }

    #[test]
    fn vector_quant_scalar_kind() {
        assert_eq!(VectorQuant::None.scalar_kind(), ScalarKind::F32);
        assert_eq!(VectorQuant::F16.scalar_kind(), ScalarKind::F16);
        assert_eq!(VectorQuant::I8.scalar_kind(), ScalarKind::I8);
    }

    #[test]
    fn vector_quant_labels() {
        assert_eq!(VectorQuant::None.label(), "f32 (none)");
        assert_eq!(VectorQuant::F16.label(), "f16");
        assert_eq!(VectorQuant::I8.label(), "i8");
    }
}
