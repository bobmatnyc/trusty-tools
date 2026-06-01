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
//! (`sample_diffs_for_batches`).  Pass 2 adds `batch_reviewer`,
//! `synthesizer`, and `reporter` plus the `profile` CLI subcommand.
//!
//! Pass 1 (data foundation) covers #561, #562, #563, #564.
//! Pass 2 (LLM + output + CLI) covers #565, #566, #567, #568.
//!
//! Test: each submodule carries its own unit-test section.

pub mod batch;
pub mod batch_reviewer;
pub mod diff_sampler;
pub mod error;
pub mod reporter;
pub mod selector;
pub mod synthesizer;
pub mod types;

// ── Re-exports for convenience ─────────────────────────────────────────────

pub use batch::{Window, assemble_period_batches};
pub use batch_reviewer::BatchReviewer;
pub use diff_sampler::{DiffSamplerConfig, MAX_DIFF_CHARS, sample_diffs_for_batches};
pub use error::{ProfileError, Result};
pub use reporter::Reporter;
pub use selector::{ContributorSelector, ResolvedIdentity, resolve_contributor, resolve_db_path};
pub use synthesizer::Synthesizer;
pub use types::{
    ContributorProfile, LongitudinalFinding, PROFILE_VERSION, PeriodBatch, SampledDiff,
    TokenCostSummary, Trajectory, TrendTag,
};
// Also re-export AuthorPeriodSummary so callers don't need to reach into tga.
pub use tga::report::period_trends::AuthorPeriodSummary;
