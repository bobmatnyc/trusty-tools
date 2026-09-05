//! Machine-tier detection and the proportional memory budget every trusty-*
//! daemon sizes itself from (#6820).
//!
//! Why: the suite's supported-hardware bar is "24 GB supported, 16 GB minimum,
//! documented degrade below" (epic #6802). Enforcing that needs ONE place that
//! answers "how much RAM is really available here" and "how many MB may I hold".
//! `trusty-search` had that code privately in `core::memory_policy`;
//! `trusty-memory` had none of it and sized its palace cap off a fixed 64
//! regardless of the host, so the two daemons could not agree on the machine
//! they shared. A second copy of these formulas is a defect — see CLAUDE.md,
//! "Common entry point, clean domain demarcation".
//! What: `detect_total_ram_mb` (the cgroup-aware RAM read), `MemoryTier`
//! (the 16/32/64 GB bands plus the new sub-16 GB `MemoryTier::Degraded`
//! posture), the four proportional formulas, and `MachineBudget` which ties
//! them together. Every number is moved verbatim from `trusty-search`, so a
//! 16/32/64/128 GB host resolves exactly as it did before; the only behaviour
//! change is that below 16 GB now names a tier instead of aborting startup.
//! What this module is NOT: per-daemon tunables. Chunk caps, embedding caches,
//! palace caps, and their env-var overrides stay in the daemon that owns them —
//! `trusty-search`'s `MemoryPolicy` layers on top of `MachineBudget`.
//! Nor is it live telemetry: `host_metrics` samples current CPU/memory
//! pressure through `sysinfo` and applies no cgroup clamp, which is a different
//! question from the one-shot startup budget answered here.
//! Test: `cargo test -p trusty-common --features machine-tier --no-fail-fast`.

mod budget;
mod compute;
mod constants;
mod detect;
mod tier;

#[cfg(test)]
mod tests;

pub use self::budget::MachineBudget;
pub use self::compute::{
    compute_index_memory_limit_mb, compute_max_batch_size, compute_max_chunks,
    compute_memory_limit_mb,
};
pub use self::constants::{
    FALLBACK_RAM_MB, INDEX_MEMORY_LIMIT_CEIL_MB, INDEX_MEMORY_LIMIT_FLOOR_MB, MAX_CHUNKS_CEIL,
    MAX_CHUNKS_FLOOR, MAX_COMPUTED_BATCH_SIZE, MEMORY_LIMIT_CEIL_MB, MEMORY_LIMIT_FLOOR_MB,
    MIN_COMPUTED_BATCH_SIZE, MINIMUM_SUPPORTED_RAM_MB, SUPPORTED_TARGET_RAM_MB,
};
pub use self::detect::detect_total_ram_mb;
pub use self::tier::MemoryTier;
