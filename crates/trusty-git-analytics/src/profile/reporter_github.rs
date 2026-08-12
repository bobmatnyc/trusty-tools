//! Publishing a profile to a per-contributor GitHub issue thread (#5465).
//!
//! Why: a profile written to a local directory is read once by whoever ran it.
//! One issue per contributor, appended to on every run, puts the longitudinal
//! record where the team already reads — and keeps each run's report next to the
//! ones before it, which is the whole point of a longitudinal profile.
//!
//! What: [`GithubIssueConfig`] names the target repository and label,
//! [`issue_title`] builds the title that doubles as the thread's identity, and
//! [`upsert_profile_issue`] hands both to
//! [`crate::collect::github::GitHubClient::upsert_issue_thread`].
//!
//! Auth: the client's existing token. `tga profile --github-issue` builds that
//! client from the `github:` config block, so the write path sends the same
//! personal access token the read path already sends. Switching the write path
//! to a GitHub App installation token is a change to how that client is
//! built — nothing in this module or in `issue_writer.rs` inspects the
//! credential.
//!
//! Test: `reporter_github_tests.rs`.

use crate::collect::github::{GitHubClient, IssueUpsert};

use super::error::Result;
use super::types::ContributorProfile;

/// Label applied to every profile issue, and searched on to find the thread.
///
/// Changing it orphans every thread opened under the old value — the next run
/// finds nothing and opens a second issue per contributor.
pub const PROFILE_ISSUE_LABEL: &str = "dev-profile";

/// Where a profile's issue thread lives.
///
/// Why: the target repository is deliberately NOT the repository the commits
/// came from — a profile spans repositories, and posting one person's review
/// into a product repo would publish it to everyone watching that repo.
/// What: owner, repo, and the label the thread carries.
/// Test: `github_issue_config_parses_a_slug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubIssueConfig {
    /// Repository owner (user or org).
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Label applied to the thread; defaults to [`PROFILE_ISSUE_LABEL`].
    pub label: String,
}

impl GithubIssueConfig {
    /// Parse an `owner/repo` slug.
    ///
    /// # Errors
    ///
    /// [`super::ProfileError::Config`] when the slug is not exactly two
    /// non-empty parts — a bare repo name would otherwise resolve to a
    /// repository nobody intended.
    ///
    /// Test: `github_issue_config_parses_a_slug`,
    /// `github_issue_config_rejects_a_bare_name`.
    pub fn from_slug(slug: &str) -> Result<Self> {
        let (owner, repo) = slug.split_once('/').ok_or_else(|| {
            super::ProfileError::Config(format!("--github-repo must be 'owner/repo', got '{slug}'"))
        })?;
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            return Err(super::ProfileError::Config(format!(
                "--github-repo must be 'owner/repo', got '{slug}'"
            )));
        }
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            label: PROFILE_ISSUE_LABEL.to_string(),
        })
    }
}

/// The title of a contributor's profile thread.
///
/// Why: the title is both what a reader sees and how the NEXT run finds this
/// thread, so it has to embed something unique to the contributor. The canonical
/// email is that, and [`upsert_profile_issue`] passes it as the search marker.
/// What: `[dev-profile] <name> <<email>>`.
/// Test: `issue_title_embeds_the_canonical_email`.
pub fn issue_title(profile: &ContributorProfile) -> String {
    format!(
        "[{PROFILE_ISSUE_LABEL}] {} <{}>",
        profile.canonical_name, profile.canonical_email
    )
}

/// Create or append to this contributor's profile issue.
///
/// Why: one thread per contributor is what makes the history readable; a run
/// that opened a new issue would scatter it.
/// What: upserts on the canonical email as the marker, posting `markdown` — the
/// same text [`super::Reporter::render`] writes to disk — as the issue body or
/// the new comment. Returns what happened.
///
/// # Errors
///
/// [`super::ProfileError::Git`] wrapping the underlying
/// [`crate::collect::errors::CollectError`] — including
/// `CollectError::GithubApi` with GitHub's own message when the token lacks
/// `issues: write` on the target repository.
///
/// Test: `reporter_github_tests` drives the whole path against a local mock.
pub async fn upsert_profile_issue(
    client: &GitHubClient,
    config: &GithubIssueConfig,
    profile: &ContributorProfile,
    markdown: &str,
) -> Result<IssueUpsert> {
    let title = issue_title(profile);
    let upsert = client
        .upsert_issue_thread(
            &config.owner,
            &config.repo,
            &config.label,
            &title,
            &profile.canonical_email,
            markdown,
        )
        .await?;
    Ok(upsert)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "reporter_github_tests.rs"]
mod tests;
