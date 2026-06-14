//! Public types and private wire shapes for the ADO PR fetcher.
//!
//! Why: decouples the stable public API (`AdoPullRequest`, `AdoPrReviewer`)
//! from the private JSON wire shapes (`PrRaw`, `ReviewerRaw`, etc.) that ADO
//! can change across preview API versions.
//! What: defines all type declarations and the `From<PrRaw> for AdoPullRequest`
//! conversion, which encodes the merge-commit-SHA gate (issue #96).
//! Test: deserialization shapes exercised by `pr_fetcher_tests.rs`;
//! `From<PrRaw>` strategy gate covered by `pr_raw_*` test functions there.

use chrono::{DateTime, Utc};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A normalized Azure DevOps pull request.
///
/// Mirrors only the subset of fields persisted in `pull_requests` /
/// `pr_reviewers`. The raw JSON shape from ADO is intentionally not exposed:
/// it changes between preview API versions and is not load-bearing for the
/// downstream report.
#[derive(Debug, Clone)]
pub struct AdoPullRequest {
    /// `pullRequestId` from ADO.
    pub pr_number: i64,
    /// Display title.
    pub title: String,
    /// Optional Markdown body. Often empty for squash merges.
    pub description: Option<String>,
    /// Author — `uniqueName` if present, otherwise `displayName`.
    pub author: String,
    /// PR creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Time the PR was closed (merged or abandoned).
    pub closed_at: Option<DateTime<Utc>>,
    /// Source branch ref (e.g. `refs/heads/feature/foo`).
    pub source_branch: String,
    /// Target branch ref (e.g. `refs/heads/main`).
    pub target_branch: String,
    /// Lifecycle status: `"active"`, `"completed"`, `"abandoned"`.
    pub status: String,
    /// Reviewer list (may be empty).
    pub reviewers: Vec<AdoPrReviewer>,
    /// Merge commit SHA from `lastMergeCommit.commitId`. `None` for PRs that
    /// have never been merged (active/abandoned, or completed via squash
    /// where ADO has not populated the field). When present, this is the
    /// commit that appears on the target branch and matches the SHA in the
    /// `commits` table — enabling the same `pull_requests.commit_shas` →
    /// `commits.sha` join the GitHub provider exposes.
    pub merge_commit_sha: Option<String>,
}

/// A single reviewer entry attached to an [`AdoPullRequest`].
#[derive(Debug, Clone)]
pub struct AdoPrReviewer {
    /// Stable identifier — `uniqueName` from ADO (e.g. `user@contoso.com`).
    pub reviewer_id: String,
    /// Display name as shown in the ADO UI.
    pub display_name: String,
    /// ADO vote value: `10` approved, `5` approved-with-suggestions, `0`
    /// no-vote, `-5` waiting-for-author, `-10` rejected.
    pub vote: i32,
    /// Whether the reviewer was marked as required for the PR.
    pub is_required: bool,
    /// `true` for AD group reviewers (e.g. `[Project]\\Reviewers`).
    pub is_container: bool,
}

// ---------------------------------------------------------------------------
// Private wire shapes (ADO JSON → Rust)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PrRaw {
    pub(super) pull_request_id: i64,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) status: String,
    #[serde(default)]
    pub(super) created_by: Option<IdentityRaw>,
    pub(super) creation_date: DateTime<Utc>,
    #[serde(default)]
    pub(super) closed_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(super) source_ref_name: String,
    #[serde(default)]
    pub(super) target_ref_name: String,
    #[serde(default)]
    pub(super) reviewers: Vec<ReviewerRaw>,
    #[serde(default)]
    pub(super) last_merge_commit: Option<CommitRefRaw>,
    /// Top-level merge strategy (`noFastForward` / `squash` / `rebase` /
    /// `rebaseMerge`). Preferred when present; otherwise we fall back to
    /// [`PrRaw::completion_options`]. See the module-level matrix.
    #[serde(default)]
    pub(super) merge_strategy: Option<String>,
    /// Nested completion metadata. Older ADO API versions only surface the
    /// merge strategy here, so we deserialize both shapes.
    #[serde(default)]
    pub(super) completion_options: Option<CompletionOptionsRaw>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommitRefRaw {
    #[serde(default)]
    pub(super) commit_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompletionOptionsRaw {
    #[serde(default)]
    pub(super) merge_strategy: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct IdentityRaw {
    #[serde(default)]
    pub(super) unique_name: Option<String>,
    #[serde(default)]
    pub(super) display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReviewerRaw {
    #[serde(default)]
    pub(super) unique_name: Option<String>,
    #[serde(default)]
    pub(super) display_name: Option<String>,
    #[serde(default)]
    pub(super) vote: i32,
    #[serde(default)]
    pub(super) is_required: bool,
    #[serde(default)]
    pub(super) is_container: bool,
}

// ---------------------------------------------------------------------------
// Conversion: wire → public
// ---------------------------------------------------------------------------

impl From<PrRaw> for AdoPullRequest {
    fn from(raw: PrRaw) -> Self {
        let author = raw
            .created_by
            .as_ref()
            .and_then(|i| i.unique_name.clone().or_else(|| i.display_name.clone()))
            .unwrap_or_default();
        let reviewers = raw
            .reviewers
            .into_iter()
            .map(|r| {
                let display = r.display_name.unwrap_or_default();
                let id = r.unique_name.unwrap_or_else(|| display.clone());
                AdoPrReviewer {
                    reviewer_id: id,
                    display_name: display,
                    vote: r.vote,
                    is_required: r.is_required,
                    is_container: r.is_container,
                }
            })
            .collect();
        // Pull the merge commit SHA from `lastMergeCommit.commitId` only
        // for *completed* PRs whose merge strategy actually preserves a
        // merge commit on the target branch (issue #96). ADO populates
        // `lastMergeCommit` even for active PRs — it's the most recent
        // merge attempt, which for unmerged PRs is a virtual preview
        // merge on `refs/pull/N/merge`, not a commit that ever landed on
        // the target branch (issue #92). For squash / rebase / rebaseMerge
        // completions the SHA likewise does not appear on the target
        // branch, so emitting it would produce non-joinable rows against
        // the `commits` table. We accept the SHA only when the strategy
        // is `noFastForward` or absent (older API versions / true merges).
        // Empty strings are treated as missing — some ADO previews return
        // `lastMergeCommit: {}`.
        let strategy_allows_merge_sha = {
            let strategy = raw.merge_strategy.as_deref().or_else(|| {
                raw.completion_options
                    .as_ref()
                    .and_then(|co| co.merge_strategy.as_deref())
            });
            match strategy {
                None => true,
                Some(s) => s.eq_ignore_ascii_case("noFastForward"),
            }
        };
        let merge_commit_sha =
            if raw.status.eq_ignore_ascii_case("completed") && strategy_allows_merge_sha {
                raw.last_merge_commit
                    .and_then(|c| c.commit_id)
                    .filter(|s| !s.is_empty())
            } else {
                None
            };
        AdoPullRequest {
            pr_number: raw.pull_request_id,
            title: raw.title,
            description: raw.description,
            author,
            created_at: raw.creation_date,
            closed_at: raw.closed_date,
            source_branch: raw.source_ref_name,
            target_branch: raw.target_ref_name,
            status: raw.status,
            reviewers,
            merge_commit_sha,
        }
    }
}
