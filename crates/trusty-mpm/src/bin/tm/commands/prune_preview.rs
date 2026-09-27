//! The per-project preview `tm session prune-worktrees` prints, and the scope
//! checks it applies to every daemon reply (#8782).
//!
//! Why: the preview printed a reclaimable COUNT and no paths, so an operator
//! could not see what `--merged-prs --force` would delete, and the command ran
//! across every registered project. The operator will not run a destructive
//! prune until the preview names each path, its project, and its reason, and
//! until the scope is the project the command was typed in.
//! What: [`preview_lines`] renders the rows the route returns, grouped by
//! project, with a total; [`planned_paths`] is the set a `--force` run hands
//! back as its allowlist; [`check_scope_echo`] refuses a reply from a daemon
//! that did not honour the requested scope.
//! Test: `prune_preview_tests`.

use std::collections::BTreeMap;

use serde_json::Value;

/// One previewed worktree, as the route reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    path: String,
    project: String,
    reason: String,
}

/// Parse an array of `{ path, project, reason }` objects; a malformed entry is
/// skipped and an absent array is empty.
fn rows(value: Option<&Value>) -> Vec<Row> {
    let field = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    value
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|v| {
                    Some(Row {
                        path: field(v, "path")?,
                        project: field(v, "project").unwrap_or_else(|| "(unknown project)".into()),
                        reason: field(v, "reason").unwrap_or_else(|| "(no reason given)".into()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The rows a reply would remove: the orphan pass's, then the merged-PR pass's.
fn removal_rows(body: &Value) -> Vec<Row> {
    let mut out = rows(body.get("orphan_rows"));
    out.extend(rows(
        body.get("merged_prs")
            .and_then(|m| m.get("reclaimable_paths")),
    ));
    out
}

/// Every path a preview lists for removal, sorted and de-duplicated (#8782).
///
/// Why: a `--force` run sends this back as `only_paths`, so it can remove
/// nothing its own preview did not list.
/// Test: `planned_paths_is_the_union_of_both_passes`.
pub(crate) fn planned_paths(body: &Value) -> Vec<String> {
    let mut paths: Vec<String> = removal_rows(body).into_iter().map(|r| r.path).collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The preview, grouped by project, one line per path, with a total (#8782).
///
/// What: a scope line; then per project a header with its counts, a `remove`
/// line per path the pass would remove and an `unknown` line per path kept
/// because its pull-request state could not be read; then a total line.
/// Test: `preview_lines_group_every_path_by_project_with_a_count`,
/// `preview_lines_report_an_empty_preview`.
pub(crate) fn preview_lines(body: &Value) -> Vec<String> {
    let remove = removal_rows(body);
    let unknown = rows(body.get("merged_prs").and_then(|m| m.get("unknown_paths")));
    let scope = body
        .get("scope")
        .and_then(|s| s.get("project_root"))
        .and_then(Value::as_str)
        .map_or_else(
            || "all registered projects".to_string(),
            |p| format!("project {p}"),
        );
    let mut by_project: BTreeMap<&str, (Vec<&Row>, Vec<&Row>)> = BTreeMap::new();
    for r in &remove {
        by_project.entry(&r.project).or_default().0.push(r);
    }
    for r in &unknown {
        by_project.entry(&r.project).or_default().1.push(r);
    }
    let mut out = vec![format!("prune-worktrees preview — scope: {scope} (#8782)")];
    for (project, (rm, unk)) in &by_project {
        out.push(format!(
            "project {project}: {} to remove, {} unknown (kept)",
            rm.len(),
            unk.len()
        ));
        out.extend(
            rm.iter()
                .map(|r| format!("  remove   {} — {}", r.path, r.reason)),
        );
        out.extend(
            unk.iter()
                .map(|r| format!("  unknown  {} — {}", r.path, r.reason)),
        );
    }
    out.push(format!(
        "total: {} worktree(s) to remove, {} kept as unknown, across {} project(s)",
        remove.len(),
        unknown.len(),
        by_project.len()
    ));
    out
}

/// Refuse a daemon reply that does not confirm the scope this run asked for
/// (#8782).
///
/// Why: the daemon is long-lived and a CLI upgrade never bounces it. A daemon
/// that predates #8782 drops `project_root` and `only_paths` silently and runs
/// daemon-global — the exact behaviour this change removes — so its reply must
/// be refused rather than printed as a scoped result.
/// What: when `required`, the reply must carry a `scope` echo. When a project
/// was requested the echo must name that same project; when none was, it must
/// name none.
/// Test: `a_reply_without_the_scope_echo_is_refused`,
/// `a_reply_scoped_to_another_project_is_refused`.
pub(crate) fn check_scope_echo(
    body: &Value,
    project_root: Option<&str>,
    required: bool,
) -> anyhow::Result<()> {
    let Some(echo) = body.get("scope") else {
        anyhow::ensure!(
            !required,
            "the daemon did not confirm the prune scope, so it predates #8782 and ran across \
             every registered project; nothing more was sent. Restart the daemon on the \
             current binary and re-run."
        );
        return Ok(());
    };
    let echoed = echo.get("project_root").and_then(Value::as_str);
    anyhow::ensure!(
        echoed == project_root,
        "the daemon scoped this prune to {} but {} was requested (#8782); nothing more was sent",
        echoed.unwrap_or("every registered project"),
        project_root.unwrap_or("every registered project"),
    );
    Ok(())
}

/// The canonical checkout `dir` belongs to, as the scope a prune from `dir`
/// is bounded to (#8782).
///
/// Why: the daemon compares canonical paths, and on macOS a temp or symlinked
/// path has two spellings; the CLI must send the one the daemon will echo.
/// What: [`trusty_mpm::session_manager::worktree_scope::project_root_for`],
/// canonicalized. Outside a repository it is an error naming
/// `--all-projects`, never a silent fall-back to every project.
/// Test: `project_root_from_a_linked_worktree_is_its_main_checkout`,
/// `project_root_from_outside_a_repository_is_refused`.
pub(crate) fn project_root_from(dir: &std::path::Path) -> anyhow::Result<String> {
    let root =
        trusty_mpm::session_manager::worktree_scope::project_root_for(dir).ok_or_else(|| {
            anyhow::anyhow!(
                "{} is not inside a git repository, so there is no project to scope \
             prune-worktrees to; pass --all-projects to act on every registered project (#8782)",
                dir.display()
            )
        })?;
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    Ok(root.to_string_lossy().into_owned())
}

/// [`project_root_from`] for the process's working directory.
pub(crate) fn current_project_root() -> anyhow::Result<String> {
    project_root_from(&std::env::current_dir()?)
}

#[cfg(test)]
#[path = "prune_preview_tests.rs"]
mod prune_preview_tests;
