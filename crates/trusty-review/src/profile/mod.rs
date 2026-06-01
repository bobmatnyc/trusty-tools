//! Longitudinal contributor-profile pipeline for trusty-review (epic #558).
//!
//! Why: single-PR code review gives a snapshot view; contributor profiling
//! aggregates weeks or months of commits into period batches so an LLM can
//! identify trends, recurring issues, and quality trajectory — information
//! not visible in any individual diff.
//! What: exposes the data models (`ContributorProfile`, `PeriodBatch`,
//! `SampledDiff`, `LongitudinalFinding`, …), the identity resolver
//! (`ContributorSelector`), the period-batch assembler
//! (`assemble_period_batches`), and the diff sampler
//! (`sample_diffs_for_batches`).  These components compose into the full
//! profile pipeline in Pass 2 (LLM narrator).
//!
//! Pass 1 (this pass) covers #561, #562, #563, #564:
//! - Data models (types.rs)
//! - Identity resolution (selector.rs)
//! - Period batch assembly (batch.rs)
//! - Diff sampling (diff_sampler.rs)
//!
//! Test: each submodule carries its own unit-test section; see the `tests`
//! blocks in types.rs, selector.rs, batch.rs, and diff_sampler.rs.

pub mod batch;
pub mod diff_sampler;
pub mod error;
pub mod selector;
pub mod types;

// ── Re-exports for convenience ─────────────────────────────────────────────

pub use batch::{Window, assemble_period_batches};
pub use diff_sampler::{DiffSamplerConfig, MAX_DIFF_CHARS, sample_diffs_for_batches};
pub use error::{ProfileError, Result};
pub use selector::{ContributorSelector, ResolvedIdentity, resolve_contributor, resolve_db_path};
pub use types::{
    ContributorProfile, LongitudinalFinding, PROFILE_VERSION, PeriodBatch, SampledDiff,
    TokenCostSummary, Trajectory, TrendTag,
};
// Also re-export AuthorPeriodSummary so callers don't need to reach into tga.
pub use tga::report::period_trends::AuthorPeriodSummary;
