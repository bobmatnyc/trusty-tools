//! Auto-tuned memory caps based on detected system RAM.
//!
//! Why: Static defaults for `TRUSTY_MAX_CHUNKS`, `TRUSTY_EMBEDDING_CACHE`,
//! `TRUSTY_MAX_BATCH_SIZE`, `TRUSTY_BM25_CORPUS_CAP`, `TRUSTY_MAX_KG_NODES`,
//! and `TRUSTY_MEMORY_LIMIT_MB` cannot fit every host: on an 8 GB laptop they
//! risk OOM; on a 192 GB workstation they're needlessly conservative. This
//! module detects total physical RAM at startup, selects a memory tier, and
//! computes sensible default caps. Env vars always override.
//! What: provides [`MemoryPolicy::detect`] which (1) reads total RAM via
//! platform-specific syscalls (`sysctl hw.memsize` on macOS, `/proc/meminfo`
//! on Linux), (2) classifies into a [`MemoryTier`], (3) starts with the
//! tier's default caps, (4) overrides any field whose env var is set, and
//! (5) writes the resolved values back into the process environment so
//! existing module-level readers pick them up automatically.
//! Test: see the `tests` module — tier selection table, env override behaviour,
//! and a smoke test that RAM detection returns a non-zero value on the host
//! running the test suite.

mod compute;
mod constants;
mod detect;
mod policy;
#[cfg(test)]
mod tests_basic;
#[cfg(test)]
mod tests_env;
mod tier;

pub use self::compute::{
    resolve_coreml_batch_size, resolve_coreml_tripwire_mb, COREML_BATCH_SIZE_MAX,
    COREML_BATCH_SIZE_MIN, DEFAULT_COREML_BATCH_SIZE, DEFAULT_COREML_TRIPWIRE_MB,
};
pub use self::detect::detect_total_ram_mb;
pub use self::policy::MemoryPolicy;
pub use self::tier::MemoryTier;
