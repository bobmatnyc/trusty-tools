//! Auto-tuned memory caps based on detected system RAM.
//!
//! Why: Static defaults for `TRUSTY_MAX_CHUNKS`, `TRUSTY_EMBEDDING_CACHE`,
//! `TRUSTY_MAX_BATCH_SIZE`, `TRUSTY_BM25_CORPUS_CAP`, `TRUSTY_MAX_KG_NODES`,
//! and `TRUSTY_MEMORY_LIMIT_MB` cannot fit every host: on an 8 GB laptop they
//! risk OOM; on a 192 GB workstation they're needlessly conservative. This
//! module detects total physical RAM at startup, selects a memory tier, and
//! computes sensible default caps. Env vars always override.
//! What: provides [`MemoryPolicy::detect`] which (1) resolves a
//! `trusty_common::machine_tier::MachineBudget` — the RAM read, the
//! [`MemoryTier`], and the two proportional soft limits — (2) starts with that
//! tier's trusty-search caps, (3) overrides any field whose env var is set, and
//! (4) writes the resolved values back into the process environment so existing
//! module-level readers pick them up automatically.
//! What moved (#6820): the RAM read, the tier bands, and the proportional
//! formulas now live in `trusty_common::machine_tier` so `trusty-memory` reads
//! the same numbers. They are RE-EXPORTED from here unchanged, so every
//! `crate::core::memory_policy::*` call site is untouched. What stayed: this
//! daemon's own caps (`tier::TierDefaults`), the env-override resolution
//! ([`MemoryPolicy`]), and the CoreML knobs (`coreml`).
//! Test: `super::tests_basic` and `super::tests_env` — tier defaults table, env
//! override behaviour, and the degrade posture below 16 GB. The shared tier
//! table itself is tested in trusty-common
//! (`cargo test -p trusty-common --features machine-tier`).

mod coreml;
mod policy;
#[cfg(test)]
mod tests_basic;
#[cfg(test)]
mod tests_env;
mod tier;

pub use self::coreml::{
    resolve_coreml_batch_size, resolve_coreml_tripwire_mb, COREML_BATCH_SIZE_MAX,
    COREML_BATCH_SIZE_MIN, DEFAULT_COREML_BATCH_SIZE, DEFAULT_COREML_TRIPWIRE_MB,
};
pub use self::policy::MemoryPolicy;

// #6820: re-exported from trusty-common rather than defined here, so
// `crate::core::memory_policy::{detect_total_ram_mb, MemoryTier}` keeps
// resolving for every existing call site (e.g. `service::embed_pool`).
pub use trusty_common::machine_tier::{detect_total_ram_mb, MemoryTier, MINIMUM_SUPPORTED_RAM_MB};
