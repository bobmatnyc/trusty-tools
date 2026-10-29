//! Attach an issue to the repo's board, and the one failure that degrades
//! (#10276, epic #10274 D8 and D10).
//!
//! Why: `tm issue board add <N>` and `tm issue epic create` both put a filed
//! issue on a board. D10 lets exactly ONE failure degrade — the token lacks the
//! project scope — to a `no-project:` comment and exit 0; a 502, an SSO 403 or
//! a deleted project is retryable or needs a human, and a comment would make a
//! transient failure a permanent record. Keeping the classifier and the
//! degrade in one function is what lets a reviewer see the degrade is limited
//! to that arm.
//! What: [`classify`] sorts attach error text into [`AttachFailure`];
//! [`attach_or_waive`] matches every arm explicitly — only
//! [`AttachFailure::MissingScope`] posts the comment (once per issue).
//! [`resolve_target`] picks the board: the `project:` block, else
//! `agents.ticketing.default_project` (a title) as the fallback D8 keeps.
//! [`add`] is the verb.
//! Test: `board/attach_tests.rs`.

use super::client::BoardClient;
use super::error::BoardError;
use super::project::ProjectBlock;
use super::snapshot::{owner_projects, split_repo};

/// The comment prefix recording why an issue carries no project.
///
/// Shaped after the standard's `no-milestone:` hatch; `tm issue epic create`
/// writes the same prefix.
pub(crate) const NO_PROJECT_PREFIX: &str = "no-project:";

/// The class of an attach failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachFailure {
    /// The token lacks the project scope — the only degrading arm (D10).
    MissingScope,
    /// An organisation's SAML SSO has not authorised the token.
    SsoRequired,
    /// The project does not resolve (deleted, or the number is wrong).
    ProjectNotFound,
    /// Anything else: a 5xx, a rate limit, a `gh` that would not run.
    Other,
}

impl AttachFailure {
    /// A short name for messages.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::MissingScope => "missing project scope",
            Self::SsoRequired => "SSO authorisation required",
            Self::ProjectNotFound => "project not found",
            Self::Other => "other failure",
        }
    }
}

/// Classify an attach error by its text.
///
/// Why: the scope refusal is told apart from everything else by gh's wording
/// (`your authentication token is missing required scopes [project]`) and the
/// raw GraphQL message it wraps (`… has not been granted the required scopes`).
/// What: SSO and not-found markers are checked FIRST, so a message carrying
/// both an SSO marker and the word "scopes" never degrades. #9750's
/// `command not found: env -u …` (the subagent identity failure) names no
/// project and no scope, so it
/// is [`AttachFailure::Other`].
/// Test: `attach_failures_are_classified_and_9750_is_not_a_scope_refusal_10276`.
pub(crate) fn classify(text: &str) -> AttachFailure {
    let lowered = text.to_lowercase();
    if lowered.contains("saml") || lowered.contains("x-github-sso") || lowered.contains(" sso") {
        AttachFailure::SsoRequired
    } else if lowered.contains("could not resolve to a projectv2")
        || lowered.contains("project not found")
    {
        AttachFailure::ProjectNotFound
    } else if lowered.contains("required scopes") || lowered.contains("missing required scope") {
        AttachFailure::MissingScope
    } else {
        AttachFailure::Other
    }
}

/// What an attach did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttachOutcome {
    /// The issue is on the board.
    Attached,
    /// D10's degrade: the scope is missing; `posted` is false when an earlier
    /// run already left the comment.
    Waived {
        /// The one-line reason.
        reason: String,
        /// Whether this run posted the comment.
        posted: bool,
    },
}

/// Attach, or — for a missing scope only — record why on the issue.
///
/// What: runs `attach`; on failure classifies it. `MissingScope` reads the
/// issue's comments and posts `no-project: <reason>` unless one exists, then
/// returns [`AttachOutcome::Waived`]. Every other arm returns
/// [`BoardError::AttachFailed`] and posts nothing. A failed comment read or
/// post is returned: that arm leaves nothing on the record.
pub(crate) fn attach_or_waive(
    issue: u64,
    number: u64,
    attach: impl FnOnce() -> Result<(), BoardError>,
    comments: impl FnOnce() -> Result<Vec<String>, BoardError>,
    post: impl FnOnce(&str) -> Result<(), BoardError>,
) -> Result<AttachOutcome, BoardError> {
    let Err(err) = attach() else {
        return Ok(AttachOutcome::Attached);
    };
    let reason = one_line(&err.to_string());
    let kind = classify(&reason);
    match kind {
        // D10: the ONLY degrading arm.
        AttachFailure::MissingScope => {
            let already = comments()?
                .iter()
                .any(|c| c.trim_start().starts_with(NO_PROJECT_PREFIX));
            if !already {
                post(&format!("{NO_PROJECT_PREFIX} {reason}"))?;
            }
            Ok(AttachOutcome::Waived {
                reason,
                posted: !already,
            })
        }
        AttachFailure::SsoRequired | AttachFailure::ProjectNotFound | AttachFailure::Other => {
            Err(BoardError::AttachFailed {
                issue,
                number,
                kind: kind.name(),
                detail: reason,
            })
        }
    }
}

/// Collapse multi-line `gh` stderr to one line.
pub(crate) fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The board an issue is attached to, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// Owner login.
    pub(crate) owner: String,
    /// Project number.
    pub(crate) number: u64,
    /// `project: block` or `default_project "<title>"`.
    pub(crate) source: String,
}

/// Pick the attach target (D8).
///
/// What: the `project:` block when present — `default_project` is then not
/// read at all. Without a block, the repo owner's project titled
/// `default_project`, which must match exactly one project.
pub(crate) fn resolve_target(
    client: &BoardClient<'_>,
    block: Option<&ProjectBlock>,
    default_project: Option<&str>,
    repo: &str,
) -> Result<Target, BoardError> {
    if let Some(block) = block {
        return Ok(Target {
            owner: block.owner.clone(),
            number: block.number,
            source: "issue-state.yaml project: block".to_string(),
        });
    }
    let Some(title) = default_project.map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(BoardError::NoAttachTarget(
            "this repo's issue-state.yaml has no `project:` block and \
             agents.ticketing.default_project is unset"
                .to_string(),
        ));
    };
    let (owner, _) = split_repo(repo);
    let matches: Vec<u64> = owner_projects(client, owner)?
        .into_iter()
        .filter(|p| p.title == title)
        .map(|p| p.number)
        .collect();
    match matches.as_slice() {
        [number] => Ok(Target {
            owner: owner.to_string(),
            number: *number,
            source: format!(
                "agents.ticketing.default_project \"{title}\" (fallback; no project: block)"
            ),
        }),
        found => Err(BoardError::NoAttachTarget(format!(
            "no project: block, and `{owner}` has {} projects titled `{title}` (need exactly one)",
            found.len()
        ))),
    }
}

/// `tm issue board add <N>`: attach issue `issue` of `repo`.
///
/// Test: `add_uses_the_project_block_and_ignores_default_project_10276`, and
/// the D10 arms in `board/attach_tests.rs`.
pub(crate) fn add(
    client: &BoardClient<'_>,
    target: &Target,
    repo: &str,
    issue: u64,
) -> Result<AttachOutcome, BoardError> {
    let n = issue.to_string();
    attach_or_waive(
        issue,
        target.number,
        || client.item_add(repo, issue, &target.owner, target.number),
        || {
            let out = client.run(
                "gh",
                &["issue", "view", &n, "--repo", repo, "--json", "comments"],
                "gh issue view --json comments",
            )?;
            let parsed: serde_json::Value =
                serde_json::from_str(&out).map_err(|e| BoardError::Parse {
                    what: "gh issue view --json comments".into(),
                    detail: e.to_string(),
                })?;
            Ok(parsed["comments"]
                .as_array()
                .map(|cs| {
                    cs.iter()
                        .filter_map(|c| c["body"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default())
        },
        |body| {
            client.run(
                "gh",
                &["issue", "comment", &n, "--repo", repo, "--body", body],
                "gh issue comment",
            )?;
            Ok(())
        },
    )
}
