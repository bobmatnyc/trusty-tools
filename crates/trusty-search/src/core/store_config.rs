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

use std::time::Duration;

use usearch::ScalarKind;

/// Environment variable selecting whether warm-booted HNSW snapshots are served
/// directly from the memory-mapped `Index::view` (low RSS) or eagerly promoted
/// to a heap-resident copy on load (higher RSS, no cold page-fault latency).
pub const HNSW_MMAP_SERVE_ENV: &str = "TRUSTY_HNSW_MMAP_SERVE";

/// Environment variable selecting the scalar precision new HNSW indexes are
/// built with. Applied only at index *creation* time; an existing snapshot
/// keeps the precision it was built with, and only the explicit backfill
/// (`trusty-search quantize`, issue #6822) converts one in place.
///
/// #6822: unset now resolves to [`VectorQuant::F16`], not `f32`. Set this to
/// `f32` (or `none`) to keep full precision for indexes built from now on.
pub const VECTOR_QUANT_ENV: &str = "TRUSTY_VECTOR_QUANT";

/// Environment variable gating the idle-sweep HNSW re-view (demotion) added
/// by issue #2164. `true` (the default) lets the shared idle sweep
/// (`server::tickers::spawn_idle_chunk_eviction_ticker`, same window as
/// `TRUSTY_CHUNKS_IDLE_EVICT_SECS`) demote a promoted-but-clean, idle HNSW
/// store back to `Index::view` (mmap), reclaiming its heap-resident copy.
/// `false` disables demotion while leaving chunk/BM25/entity eviction
/// untouched — an escape hatch in case the view↔load↔view cycle proves
/// riskier than the memory it reclaims on some deployment. #6826: it gates
/// BOTH demote paths, the write-cooldown one included — this is the kill
/// switch for the mechanism, not for one trigger.
pub const HNSW_REVIEW_IDLE_ENV: &str = "TRUSTY_HNSW_REVIEW_IDLE";

/// Environment variable setting the WRITE-idle cooldown, in seconds, after
/// which a promoted-and-written HNSW store is persisted and demoted back to
/// `Index::view` (issue #6826).
///
/// Covers a different TRIGGER from [`HNSW_REVIEW_IDLE_ENV`]: that knob's
/// #2164 sweep demotes a store whose on-disk snapshot is ALREADY
/// byte-identical to the graph, which a written store never is until
/// something saves it. This knob covers the written store — the case behind
/// the measured 76 MB mmap-resident against 9 GB of heap on the 128 GB
/// reference host.
///
/// It is NOT an independent switch. [`HNSW_REVIEW_IDLE_ENV`] disables
/// heap→view demotion as a mechanism, this path included; turning it off
/// disables both. This knob is the fine-grained switch that turns off only the
/// write-cooldown path.
pub const HNSW_DEMOTE_COOLDOWN_SECS_ENV: &str = "TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS";

/// Default write-idle cooldown before a written HNSW store is persisted and
/// demoted back to a view: 5 minutes.
///
/// Why 300 s: the demote costs one `save()` (an FFI serialize of the whole
/// graph) plus an `Index::view`, and the next write pays a re-promote
/// (`Index::load`). Five minutes is long enough that an editor session's
/// edit-commit-edit rhythm never crosses it — the file watcher commits far
/// more often than that while a project is being worked on — and short enough
/// that a project left alone stops holding its heap copy.
pub const HNSW_DEMOTE_COOLDOWN_SECS_DEFAULT: u64 = 300;

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

/// Scalar precision a new HNSW index is built with (issue #709, quick win #2;
/// default flipped to `F16` by issue #6822).
///
/// Why: usearch can store vectors at reduced precision, trading a small recall
/// loss for a large reduction in resident + on-disk footprint. `F16` halves the
/// per-vector bytes (≈2× smaller), `I8` quarters them (≈4× smaller).
///
/// Why `F16` is the default (issue #6822): the `ooc_quick_wins` fixture measures
/// recall@10 = 1.00 for f16 against the same 1.00 f32 baseline — no measured
/// loss — so leaving every new index at `f32` spent twice the vector bytes for
/// nothing. `I8` stays opt-in: its measured recall (≈0.96 in the same fixture)
/// is a real cost an operator must choose.
/// What: a three-state enum resolved from [`VECTOR_QUANT_ENV`], mapped onto
/// usearch's [`ScalarKind`] via [`Self::scalar_kind`]. The HNSW `search` API
/// still takes `&[f32]` queries regardless of internal precision — usearch
/// quantizes the query internally — so only the index build options change.
///
/// Scope of the default: index **creation** only. An existing snapshot records
/// its own scalar kind in its header, and usearch's `load`/`view` rebuild the
/// metric and casts from that header — so opening an f32 index under the f16
/// default reads it as f32 and rewrites nothing. Converting one is the explicit
/// operator action [`super::store::UsearchStore::requantize`] performs.
/// Test: `tests::vector_quant_*`, plus
/// `tests/vector_quant_default_6822.rs::default_quant_for_a_new_index_is_f16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorQuant {
    /// Full 32-bit float precision — no recall loss. Opt in with
    /// `TRUSTY_VECTOR_QUANT=f32`.
    None,
    /// 16-bit half precision — ≈2× smaller vectors, no measured recall loss.
    /// **Default** since issue #6822.
    #[default]
    F16,
    /// 8-bit integer quantization — ≈4× smaller vectors, larger recall cost.
    I8,
}

impl VectorQuant {
    /// Resolve the quantization kind from [`VECTOR_QUANT_ENV`].
    ///
    /// Why: centralises the operator-facing string → enum mapping so index
    /// creation has a single source of truth.
    /// What: unset or empty → the default (`F16` since #6822); `none` / `f32` /
    /// `fp32` / `full` → `None`; `f16` / `fp16` / `half` → `F16`; `i8` / `int8`
    /// → `I8` (case-insensitive, trimmed). Any other value falls back to the
    /// default with a `tracing::warn!`.
    /// Test: `tests::vector_quant_parse_*`.
    pub fn from_env() -> Self {
        match std::env::var(VECTOR_QUANT_ENV) {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::default(),
        }
    }

    /// Pure parser split out from [`Self::from_env`] for testability.
    ///
    /// #6822: an empty value means "unset" and now resolves to the default
    /// (`F16`) rather than to `f32` — a shell that exports the variable empty
    /// must not silently opt out of the new default. Only the explicit `f32` /
    /// `none` spellings select full precision.
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" => Self::default(),
            "none" | "f32" | "fp32" | "full" => Self::None,
            "f16" | "fp16" | "half" => Self::F16,
            "i8" | "int8" => Self::I8,
            other => {
                tracing::warn!(
                    "{VECTOR_QUANT_ENV}={other:?} is not a recognised quantization kind \
                     (expected f32|f16|i8); defaulting to {}",
                    Self::default().label()
                );
                Self::default()
            }
        }
    }

    /// Strict parse of an operator-supplied precision, for a REQUEST rather
    /// than an env var.
    ///
    /// Why (#6822): [`Self::parse`] deliberately degrades an unrecognised env
    /// value to the default with a warning — an unset-ish environment must
    /// never fail a daemon start. A request is the opposite case: an operator
    /// who types `--to fp8` on a one-way, whole-arena conversion must be told
    /// it is wrong, not silently given f16.
    /// What: the same spellings [`Self::parse`] accepts, minus the empty and
    /// fallback arms; anything else is `None` for the caller to reject.
    /// Test: `tests::vector_quant_parse_operator_value_rejects_garbage`.
    pub fn parse_operator_value(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" | "f32" | "fp32" | "full" => Some(Self::None),
            "f16" | "fp16" | "half" => Some(Self::F16),
            "i8" | "int8" => Some(Self::I8),
            _ => Option::None,
        }
    }

    /// Recover the quantization kind an already-built index actually holds.
    ///
    /// Why (#6822): reporting whether an index is quantized — on
    /// `GET /indexes/:id/status` and in the backfill's dry run — has to read
    /// the LIVE index, never the env var. The env var says what the NEXT index
    /// will be built with; a warm-booted snapshot carries its own scalar kind
    /// in its header and usearch restores the metric from there.
    /// What: the inverse of [`Self::scalar_kind`]. Returns `None` for a scalar
    /// kind this knob cannot express (usearch supports several this crate never
    /// builds), so a caller reports "unknown" rather than guessing.
    /// Test: `tests::vector_quant_round_trips_through_scalar_kind`.
    pub fn from_scalar_kind(kind: ScalarKind) -> Option<Self> {
        match kind {
            ScalarKind::F32 => Some(Self::None),
            ScalarKind::F16 => Some(Self::F16),
            ScalarKind::I8 => Some(Self::I8),
            _ => Option::None,
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

/// `true` (default) when the idle sweep should demote idle, clean, promoted
/// HNSW stores back to mmap-view mode; resolved from [`HNSW_REVIEW_IDLE_ENV`]
/// (issue #2164).
///
/// Why: operators who want the memory reclamation but are wary of a new
/// code path touching a hot HNSW index can turn just this piece off without
/// losing chunk/BM25/entity idle eviction (issue #2162), which stays on its
/// own always-on path.
/// What: unset / `1` / `true` / `yes` / `on` (any case, trimmed) → enabled
/// (the default); `0` / `false` / `no` / `off` → disabled. Any other value is
/// treated as enabled with a `tracing::warn!` so a typo never silently
/// disables the reclamation.
/// Test: `tests::hnsw_review_idle_enabled_*`.
pub fn hnsw_review_idle_enabled() -> bool {
    match std::env::var(HNSW_REVIEW_IDLE_ENV) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "" | "1" | "true" | "yes" | "on" | "enabled" => true,
            "0" | "false" | "no" | "off" | "disabled" => false,
            other => {
                tracing::warn!(
                    "{HNSW_REVIEW_IDLE_ENV}={other:?} is not a recognised boolean; \
                     defaulting to enabled"
                );
                true
            }
        },
        Err(_) => true,
    }
}

/// Write-idle cooldown before a written, promoted HNSW store is persisted and
/// demoted back to mmap-view mode; resolved from
/// [`HNSW_DEMOTE_COOLDOWN_SECS_ENV`] (issue #6826).
///
/// Why: the #2164 demote only fires on a store whose graph already matches
/// disk, so a store that has taken even one write stays heap-resident until
/// the daemon exits. Demoting a WRITTEN store means saving it first, which is
/// expensive enough that it must not fire while the project is being edited —
/// hence a cooldown measured from the last WRITE rather than the last query.
/// What: unset → [`HNSW_DEMOTE_COOLDOWN_SECS_DEFAULT`]. `0`, `off`, `false`,
/// `no`, `disabled`, or `none` → `None` (feature disabled). Any other
/// unparseable value falls back to the default with a `tracing::warn!` so a
/// typo never silently disables the reclamation.
/// Test: `tests::hnsw_demote_cooldown_parse_accepts_seconds`,
/// `tests::hnsw_demote_cooldown_parse_disables_on_zero_and_off`,
/// `tests::hnsw_demote_cooldown_parse_falls_back_on_garbage`.
pub fn hnsw_demote_cooldown() -> Option<Duration> {
    match std::env::var(HNSW_DEMOTE_COOLDOWN_SECS_ENV) {
        Ok(raw) => parse_demote_cooldown(&raw),
        Err(_) => Some(Duration::from_secs(HNSW_DEMOTE_COOLDOWN_SECS_DEFAULT)),
    }
}

/// Pure parser split out of [`hnsw_demote_cooldown`] so every accepted and
/// rejected spelling is testable without touching the process environment.
fn parse_demote_cooldown(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim().to_ascii_lowercase();
    match trimmed.as_str() {
        "" => return Some(Duration::from_secs(HNSW_DEMOTE_COOLDOWN_SECS_DEFAULT)),
        "0" | "off" | "false" | "no" | "disabled" | "none" => return Option::None,
        _ => {}
    }
    match trimmed.parse::<u64>() {
        Ok(0) => Option::None,
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => {
            tracing::warn!(
                "{HNSW_DEMOTE_COOLDOWN_SECS_ENV}={raw:?} is not a whole number of seconds; \
                 defaulting to {HNSW_DEMOTE_COOLDOWN_SECS_DEFAULT}s"
            );
            Some(Duration::from_secs(HNSW_DEMOTE_COOLDOWN_SECS_DEFAULT))
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

    /// #6822: the default is `F16`, and an EMPTY value means "unset" rather
    /// than "full precision" — a shell exporting the variable empty must not
    /// silently opt out of the new default.
    #[test]
    fn vector_quant_default_is_f16() {
        assert_eq!(VectorQuant::default(), VectorQuant::F16);
        assert_eq!(VectorQuant::default().scalar_kind(), ScalarKind::F16);
        assert_eq!(VectorQuant::parse(""), VectorQuant::F16);
        assert_eq!(VectorQuant::parse("   "), VectorQuant::F16);
    }

    #[test]
    fn vector_quant_parse_spellings() {
        for s in ["none", "f32", "FP32", " full "] {
            assert_eq!(VectorQuant::parse(s), VectorQuant::None, "{s:?}");
        }
        for s in ["f16", "FP16", " half "] {
            assert_eq!(VectorQuant::parse(s), VectorQuant::F16, "{s:?}");
        }
        for s in ["i8", "INT8", " i8 "] {
            assert_eq!(VectorQuant::parse(s), VectorQuant::I8, "{s:?}");
        }
    }

    /// #6822: an unrecognised spelling falls back to the DEFAULT, which is now
    /// f16 — the warn line names it so a typo is visible in the log.
    #[test]
    fn vector_quant_parse_garbage_defaults_to_f16() {
        assert_eq!(VectorQuant::parse("bf16"), VectorQuant::F16);
        assert_eq!(VectorQuant::parse("q4"), VectorQuant::F16);
    }

    /// #6822: an operator-supplied value on a one-way whole-arena conversion is
    /// rejected, never silently defaulted the way an env var is.
    #[test]
    fn vector_quant_parse_operator_value_rejects_garbage() {
        assert_eq!(
            VectorQuant::parse_operator_value("f16"),
            Some(VectorQuant::F16)
        );
        assert_eq!(
            VectorQuant::parse_operator_value(" F32 "),
            Some(VectorQuant::None)
        );
        assert_eq!(
            VectorQuant::parse_operator_value("int8"),
            Some(VectorQuant::I8)
        );
        for bad in ["", "  ", "fp8", "bf16", "q4"] {
            assert_eq!(VectorQuant::parse_operator_value(bad), None, "{bad:?}");
        }
    }

    /// #6822: `from_scalar_kind` is how a live index's actual precision is
    /// reported, so it must invert `scalar_kind` exactly for all three states.
    #[test]
    fn vector_quant_round_trips_through_scalar_kind() {
        for q in [VectorQuant::None, VectorQuant::F16, VectorQuant::I8] {
            assert_eq!(VectorQuant::from_scalar_kind(q.scalar_kind()), Some(q));
        }
        assert_eq!(VectorQuant::from_scalar_kind(ScalarKind::F64), None);
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

    // #6826: the cooldown parser is pure, so it is tested directly rather than
    // through the process environment — no env mutation, no cross-test race.
    #[test]
    fn hnsw_demote_cooldown_parse_accepts_seconds() {
        assert_eq!(parse_demote_cooldown("45"), Some(Duration::from_secs(45)));
        assert_eq!(
            parse_demote_cooldown("  600 "),
            Some(Duration::from_secs(600))
        );
    }

    #[test]
    fn hnsw_demote_cooldown_parse_disables_on_zero_and_off() {
        for raw in ["0", "off", "OFF", "false", "no", "disabled", "none"] {
            assert_eq!(parse_demote_cooldown(raw), None, "{raw} should disable");
        }
    }

    #[test]
    fn hnsw_demote_cooldown_parse_falls_back_on_garbage() {
        let default = Some(Duration::from_secs(HNSW_DEMOTE_COOLDOWN_SECS_DEFAULT));
        assert_eq!(parse_demote_cooldown(""), default);
        assert_eq!(parse_demote_cooldown("five minutes"), default);
        assert_eq!(parse_demote_cooldown("-1"), default);
    }

    // #3769: `hnsw_review_idle_enabled_default_and_env_override` lived here and
    // was the second writer of `TRUSTY_HNSW_REVIEW_IDLE` inside this lib test
    // binary, racing `hnsw_idle_demotion_reviews_clean_promoted_store` through
    // the same gate. It now lives in `tests/hnsw_review_idle_env.rs`, its own
    // test BINARY, alongside the other mutator. See that file's module docs.
}
