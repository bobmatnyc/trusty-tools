//! Domain models corresponding to the v1 database schema.
//!
//! These structs are the in-memory representation of rows in the core
//! tables. They are intentionally serialization-friendly via `serde` so
//! that they can be emitted as JSON in reports without an intermediate
//! DTO layer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single commit observed in a repository.
///
/// Why: rows in the `commits` SQLite table need a typed in-memory
/// counterpart that both extractors and aggregators can share.
/// What: maps 1:1 onto the v1 `commits` schema. `Serialize`/`Deserialize`
/// derives let report formatters emit it as JSON without a DTO layer.
/// Test: covered indirectly by every test that inserts into the
/// `commits` table (see `core::tests::database_opens_with_wal_and_migrations_apply`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Full git OID (hex).
    pub sha: String,

    /// Foreign key into [`Author`].
    pub author_id: Option<i64>,

    /// Author display name as recorded in the commit.
    pub author_name: String,

    /// Author email as recorded in the commit.
    pub author_email: String,

    /// Author timestamp (UTC).
    pub timestamp: DateTime<Utc>,

    /// Commit message body (raw).
    pub message: String,

    /// Repository identifier (path or canonical name).
    pub repository: String,

    /// Number of files changed.
    pub files_changed: u32,

    /// Lines added.
    pub insertions: u32,

    /// Lines deleted.
    pub deletions: u32,

    /// Foreign key into [`Classification`], if classified.
    pub classification_id: Option<i64>,

    /// Confidence assigned by the classifier (0.0–1.0).
    pub confidence: Option<f64>,

    /// True for merge commits (parents > 1).
    pub is_merge: bool,

    /// True if the commit message references a known ticket system
    /// (JIRA/Linear-style `PROJ-123`, GitHub `fixes #123`, or bare `#123`).
    ///
    /// Computed at extraction time by [`crate::collect::ticket::is_ticketed`]
    /// and persisted on the `commits` row.
    pub ticketed: bool,
}

/// A canonical author / developer identity.
///
/// Why: the same physical developer often commits under multiple
/// `(name, email)` pairs; the `authors` table holds one row per resolved
/// identity so reports collapse them.
/// What: maps to the `authors` v1 schema with the alias list stored as a
/// JSON-encoded string.
/// Test: covered by `collect::identity::resolver` tests that exercise the
/// upsert path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Canonical display name.
    pub canonical_name: String,

    /// Canonical email address.
    pub canonical_email: String,

    /// JSON-encoded array of alias strings (names or emails).
    pub aliases: String,
}

/// A classification verdict produced by the cascade.
///
/// Why: classifications are stored once per unique outcome and referenced
/// by `commits.classification_id`, so the same `(category, subcategory,
/// method)` triple is not duplicated per commit.
/// What: maps to the `classifications` v1 schema; `method` records which
/// cascade tier produced the verdict.
/// Test: covered by `classify::pipeline` tests that exercise full-cascade
/// runs against an in-memory DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Top-level category (e.g. `feature`, `bugfix`, `chore`).
    pub category: String,

    /// Optional finer-grained label.
    pub subcategory: Option<String>,

    /// Associated ticket identifier (e.g. `API-123`), if any.
    pub ticket_id: Option<String>,

    /// Confidence in this verdict (0.0–1.0).
    pub confidence: f64,

    /// Which tier of the cascade produced this verdict.
    pub method: ClassificationMethod,
}

/// File-level change record attached to a commit.
///
/// Why: per-file change data feeds the "files churned" and
/// "complexity" metrics; the per-file granularity must survive
/// round-tripping through SQLite.
/// What: maps to the `files` v1 schema with a typed `change_type`
/// (added / modified / deleted / renamed).
/// Test: covered indirectly by the git-extractor tests
/// (`collect::git::extractor`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Foreign key into [`Commit`].
    pub commit_id: i64,

    /// Relative path within the repository.
    pub path: String,

    /// Type of change.
    pub change_type: ChangeType,

    /// Lines added in this file.
    pub insertions: u32,

    /// Lines deleted in this file.
    pub deletions: u32,
}

/// A pull request record (typically GitHub).
///
/// Why: PR data drives the velocity / DORA lead-time / cycle-time
/// metrics; storing the full PR row lets us recompute those metrics
/// without re-fetching from the provider.
/// What: maps to the `pull_requests` v1 schema. Provider-specific PR
/// numbering means the `(provider, pr_number, repository)` triple is
/// the persistence-level unique identity.
/// Test: covered by `collect::github::client` and
/// `collect::bitbucket::client` tests that round-trip PR data through
/// the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    /// Primary key (database-assigned).
    pub id: i64,

    /// PR number within its repository.
    pub pr_number: u64,

    /// Repository this PR belongs to. Together with `provider` and
    /// `pr_number` this forms the persistence-level unique identity of a
    /// PR. GitHub assigns `pr_number` per-repository (so #1 in repo A is
    /// not the same PR as #1 in repo B); without this field the
    /// `(provider, pr_number)` unique index from migration v10 silently
    /// dropped cross-repo collisions during multi-repo collection (#88).
    ///
    /// Format is provider-specific:
    /// - GitHub: `"owner/repo"` (e.g. `"acme/widgets"`)
    /// - Bitbucket: `"workspace/repo_slug"`
    /// - Azure DevOps: `"project"` (PRs are project-scoped)
    pub repository: String,

    /// PR title.
    pub title: String,

    /// Author login.
    pub author: String,

    /// Lifecycle state.
    pub state: PrState,

    /// PR creation timestamp (UTC).
    pub created_at: DateTime<Utc>,

    /// Merge timestamp, if merged.
    pub merged_at: Option<DateTime<Utc>>,

    /// JSON-encoded array of commit SHAs in the PR.
    pub commit_shas: String,
}

/// Cascade tier that produced a classification.
///
/// Why: knowing which tier of the four-tier cascade produced a verdict
/// lets analytics tools surface low-confidence verdicts (e.g. the
/// catch-all routes through `LlmFallback` when LLM is enabled).
/// What: enum tagged with snake_case string values for DB persistence.
/// Test: covered by `classify::tests::engine_classify_batch_does_not_panic`
/// which asserts the cascade reports the correct tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationMethod {
    /// Matched a deterministic exact rule.
    ExactRule,
    /// Matched a regex rule.
    RegexRule,
    /// Matched via fuzzy similarity.
    FuzzyMatch,
    /// Assigned by an LLM fallback.
    LlmFallback,
    /// Set manually by a user override.
    Manual,
    /// Derived from an external ticket source (JIRA issue type or GitHub
    /// Issues label). Added for issue #260.
    ExternalSource,
    /// Composed from multiple weak signals via a weighted-sum model.
    ///
    /// Tier 2.5 — sits between the regex tier (Tier 2) and the fuzzy tier
    /// (Tier 3). Blends keyword-density, ticket-prefix presence, message-length
    /// bucket, merge indicator, and file-path signals into per-category scores.
    /// Added for issue #270.
    WeightedSum,
    /// Derived from the catch-all rule (lowest-priority, confidence 0.3).
    ///
    /// Why: distinguishes the explicit catch-all from other fuzzy verdicts so
    /// downstream consumers can filter "true unknowns" separately.
    /// Added for issue #445 batch C.
    CatchAll,
    /// Applied by the `repo_categories` fallback tier (Tier 5, #445 batch C).
    ///
    /// Why: lets callers distinguish a confident classification from a
    /// repo-default assignment, enabling metric-level filtering.
    RepoCategoryFallback,
}

impl ClassificationMethod {
    /// Stable string representation used for DB storage.
    ///
    /// Why: rusqlite needs a `&str` to bind to the `method` column; the
    /// values must stay stable across releases so existing rows continue
    /// to round-trip correctly.
    /// What: returns the lowercase snake_case label for each variant.
    /// Test: covered indirectly by every classification test that reads
    /// or writes the `classifications` table.
    pub fn as_str(&self) -> &'static str {
        match self {
            ClassificationMethod::ExactRule => "exact_rule",
            ClassificationMethod::RegexRule => "regex_rule",
            ClassificationMethod::FuzzyMatch => "fuzzy_match",
            ClassificationMethod::LlmFallback => "llm_fallback",
            ClassificationMethod::Manual => "manual",
            ClassificationMethod::ExternalSource => "external_source",
            ClassificationMethod::WeightedSum => "weighted_sum",
            ClassificationMethod::CatchAll => "catch_all",
            ClassificationMethod::RepoCategoryFallback => "repo_category_fallback",
        }
    }
}

/// File change kind for [`FileChange`].
///
/// Why: distinguishing add / modify / delete / rename lets reports
/// separate "new code" from "code shuffled around" without re-parsing
/// the git diff.
/// What: 4-variant enum with snake_case strings for DB persistence.
/// Test: covered by `collect::git::diff` extractor tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// File was added.
    Added,
    /// File contents were modified.
    Modified,
    /// File was deleted.
    Deleted,
    /// File was renamed (and possibly modified).
    Renamed,
}

impl ChangeType {
    /// Stable string representation used for DB storage.
    ///
    /// Why: see [`ClassificationMethod::as_str`] — same persistence
    /// invariant applies.
    /// What: returns the snake_case label for each variant.
    /// Test: covered by `collect::git::diff` tests.
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeType::Added => "added",
            ChangeType::Modified => "modified",
            ChangeType::Deleted => "deleted",
            ChangeType::Renamed => "renamed",
        }
    }
}

/// Lifecycle state of a [`PullRequest`].
///
/// Why: cycle-time and DORA lead-time only apply to merged PRs;
/// surfacing the state lets reports filter without joining extra tables.
/// What: open / closed / merged tri-state with snake_case persistence.
/// Test: covered by `collect::github::client` round-trip tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    /// PR is open.
    Open,
    /// PR was closed without merging.
    Closed,
    /// PR was merged.
    Merged,
}

impl PrState {
    /// Stable string representation used for DB storage.
    ///
    /// Why: see [`ClassificationMethod::as_str`] — same persistence
    /// invariant applies.
    /// What: returns the snake_case label for each variant.
    /// Test: covered by `collect::github::client` round-trip tests.
    pub fn as_str(&self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
        }
    }
}
