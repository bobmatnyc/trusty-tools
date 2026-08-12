//! Longitudinal contributor profiling.
//!
//! Why: a single code review sees one change. Aggregating months of a
//! contributor's commits into period batches shows what a per-PR view cannot —
//! which issues keep coming back, which stopped, and which way quality is
//! moving. Contributor profiling is tga's domain: it is the crate that already
//! owns commit history, identity resolution, and period trends (#5468).
//!
//! What: the pipeline runs in stages, each usable on its own:
//!
//! 1. [`selector`] — turn a login, name, or email into a canonical identity.
//! 2. [`batch`] — bucket that contributor's history into [`PeriodBatch`] windows.
//! 3. [`diff_sampler`] — attach a stratified sample of real diffs per period.
//! 4. [`batch_reviewer`] — build each period's review prompt and parse its answer.
//! 5. [`synthesizer`] — tag findings across periods and derive the trajectory.
//! 6. [`reporter`] — render JSON and Markdown.
//!
//! Stages 1–3 and 5–6 are fully deterministic. Stage 4 supplies the prompt text
//! and the response parser but not the transport that carries them to a model —
//! that arrives in #5464, on top of `trusty_common::inference`, followed by the
//! GitHub write path and the `tga profile` subcommand in #5465. A run that skips
//! the model still produces a complete profile with a fallback narrative.
//!
//! Ported from trusty-review's `src/profile/` under #5463. The port drops that
//! crate's `Finding`/`Effort`, LLM client, review config, and GitHub client; see
//! [`types::finding`] for why the finding DTO is tga-native.
//!
//! Test: each submodule carries its own tests; see the `Test:` pointers there.

pub mod batch;
pub mod batch_reviewer;
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
pub use selector::{
    resolve_contributor, resolve_db_path, ContributorSelector, ResolvedIdentity, ENV_TGA_DB,
};
pub use synthesizer::{apply_deterministic_synthesis, assign_trend_tags, derive_trajectory};
pub use types::{
    AuthorPeriodSummary, ContributorProfile, Effort, Finding, LongitudinalFinding, PeriodBatch,
    SampledDiff, TokenCostSummary, Trajectory, TrendTag, PROFILE_VERSION,
};
