//! Which of the owner's GitHub Projects are closed (#9749).
//!
//! Why: an issue filed before a project was closed still names it, and
//! `gh pr edit --add-project` cannot resolve a closed project's title. `tm pr
//! open` inherited that title and exited 3 on every PR linking such an issue
//! (projects #22 and #25, superseded by #45). Telling a closed project apart
//! from any other failure is what lets the open skip it instead.
//! What: [`closed_titles`] reads the owner — from `--repo`, else `gh repo
//! view` — then `gh project list --owner <o> --closed`, and returns the titles
//! of the closed projects.
//! Test: `pr_9749_a_closed_inherited_project_does_not_fail_the_open`,
//! `pr_10276_a_closed_project_is_skipped_and_no_default_project_is_added`.

use serde::Deserialize;

use super::{GhRunner, argv};

/// Cap passed to `gh project list -L`; gh's silent default is 30.
const PROJECT_LIST_LIMIT: &str = "200";

/// One row of `gh project list --format json`.
#[derive(Debug, Deserialize)]
struct Project {
    #[serde(default)]
    title: String,
    #[serde(default)]
    closed: bool,
}

/// The `gh project list --format json` envelope.
#[derive(Debug, Deserialize)]
struct ProjectList {
    #[serde(default)]
    projects: Vec<Project>,
}

/// The titles of the owner's closed projects.
///
/// Why: see the module doc. Called only after a project step already failed,
/// so a PR whose projects all resolve pays no extra `gh` call.
/// What: owner from `repo` (`owner/name`) when given, else `gh repo view
/// --json owner --jq .owner.login`; then the `--closed` listing, filtered to
/// the rows that are closed. Any failure is an error; the caller then treats
/// the project as an ordinary failed step.
/// Test: see the module doc.
pub(crate) fn closed_titles<R: GhRunner>(
    gh: &R,
    repo: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let named = repo
        .and_then(|r| r.trim().split_once('/'))
        .map(|(owner, _)| owner.trim().to_string())
        .filter(|o| !o.is_empty());
    let owner = match named {
        Some(owner) => owner,
        None => {
            let a = argv(&["repo", "view", "--json", "owner", "--jq", ".owner.login"]);
            gh.run(&a)?.stdout_ok(&a)?.trim().to_string()
        }
    };
    anyhow::ensure!(!owner.is_empty(), "the repository owner could not be read");
    let a = argv(&[
        "project",
        "list",
        "--owner",
        &owner,
        "--closed",
        "-L",
        PROJECT_LIST_LIMIT,
        "--format",
        "json",
    ]);
    let text = gh.run(&a)?.stdout_ok(&a)?;
    let list: ProjectList = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("`gh project list --owner {owner}` was unreadable: {e}"))?;
    Ok(list
        .projects
        .into_iter()
        .filter(|p| p.closed)
        .map(|p| p.title.trim().to_string())
        .collect())
}
