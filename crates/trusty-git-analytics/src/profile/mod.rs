//! Longitudinal contributor profiling.
//!
//! Why: a single code review is a snapshot. Aggregating months of a
//! contributor's commits into period batches is what makes recurring issues,
//! resolved ones, and quality direction visible at all — none of which any
//! individual diff shows. Profiling reads git history, identity, effort, and
//! classification data, all of which tga already owns, which is why it lives
//! here (#5463, epic #5468).
//! What: the pipeline runs
//! [`selector`] → [`batch`] → [`diff_sampler`] → [`synthesizer`] → [`reporter`]:
//! resolve who the contributor is, split their history into periods, attach
//! representative diffs, tag findings with their cross-period trend, and write
//! JSON and Markdown.
//!
//! Two stages are deliberately absent and land in follow-on work: the
//! model-written narrative (#5464) and publishing the profile to a GitHub issue
//! (#5465). Everything here is deterministic and makes no network calls.
//! Test: each submodule carries its own tests; start with
//! `synthesizer::tests::synthesize_deterministic_writes_narrative` for the
//! end-to-end deterministic shape.

pub mod batch;
pub mod diff_sampler;
pub mod error;
pub mod reporter;
pub mod selector;
pub mod synthesizer;
pub mod types;

pub use batch::{assemble_period_batches, Window};
pub use diff_sampler::{sample_diffs_for_batches, DiffSamplerConfig, MAX_DIFF_CHARS};
pub use error::{ProfileError, Result};
pub use reporter::{render_markdown, ReportFormat, Reporter};
pub use selector::{resolve_contributor, ContributorSelector, ResolvedIdentity};
pub use synthesizer::{
    assign_trend_tags, derive_trajectory, deterministic_narrative, synthesize_deterministic,
};
pub use types::{
    AuthorPeriodSummary, ContributorProfile, FindingEffort, LongitudinalFinding, PeriodBatch,
    ProfileFinding, SampledDiff, TokenCostSummary, Trajectory, TrendTag, PROFILE_VERSION,
};
