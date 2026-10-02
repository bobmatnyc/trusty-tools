//! The per-project preview `tm session prune-worktrees` prints, and the scope
//! checks it applies to every daemon reply (#8782).
//!
//! Why: the preview printed a reclaimable COUNT and no paths, so an operator
//! could not see what `--merged-prs --force` would delete, and the command ran
//! across every registered project. The operator will not run a destructive
//! prune until the preview names each path, its project, and its reason, and
//! until the scope is the project the command was typed in.
//! What: [`preview_lines`] renders the rows the route returns, grouped by
//! project, with a total; [`planned_paths`] is the per-pass sets a `--force`
//! run hands back as its allowlists; [`check_scope_echo`] refuses a reply from
//! a daemon that did not honour the requested scope, and
//! [`check_allowlist_echo`] one that would not honour the allowlists.
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

/// The merged-PR pass's reclaim rows.
fn merged_rows(body: &Value) -> Vec<Row> {
    rows(
        body.get("merged_prs")
            .and_then(|m| m.get("reclaimable_paths")),
    )
}

/// The rows a reply would remove: the orphan pass's, then the merged-PR pass's.
fn removal_rows(body: &Value) -> Vec<Row> {
    let mut out = rows(body.get("orphan_rows"));
    out.extend(merged_rows(body));
    out
}

/// What a preview listed for removal, one allowlist per pass (#8782).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlannedPaths {
    /// Sent as `only_orphan_paths`.
    pub(crate) orphan: Vec<String>,
    /// Sent as `only_merged_paths`.
    pub(crate) merged: Vec<String>,
    /// Sent as `only_discard_paths`: the orphan rows the preview marked as
    /// discarding unsaved work. `--discard-dirty` removes no other dirty tree.
    pub(crate) discard: Vec<String>,
}

impl PlannedPaths {
    /// Whether the preview listed nothing for either pass.
    pub(crate) fn is_empty(&self) -> bool {
        self.orphan.is_empty() && self.merged.is_empty()
    }
}

/// Every path a preview lists for removal, per pass, sorted and de-duplicated
/// (#8782).
///
/// Why: a `--force` run sends each list back as that pass's allowlist, so
/// neither pass can remove a path the preview listed only for the other, and
/// `--discard-dirty` discards only the work the preview named.
/// What: `discard` is the orphan rows carrying `discards_unsaved_work: true`;
/// a row without the field is not in it, so the daemon keeps that tree if it
/// is dirty.
/// Test: `planned_paths_keeps_each_pass_to_its_own_rows`.
pub(crate) fn planned_paths(body: &Value) -> PlannedPaths {
    let sorted = |mut paths: Vec<String>| {
        paths.sort();
        paths.dedup();
        paths
    };
    let paths = |rows: Vec<Row>| rows.into_iter().map(|r| r.path).collect::<Vec<_>>();
    let discard = body
        .get("orphan_rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| r.get("discards_unsaved_work").and_then(Value::as_bool) == Some(true))
        .filter_map(|r| r.get("path").and_then(Value::as_str).map(str::to_owned))
        .collect();
    PlannedPaths {
        orphan: sorted(paths(rows(body.get("orphan_rows")))),
        merged: sorted(paths(merged_rows(body))),
        discard: sorted(discard),
    }
}

/// The lines a `--force` reply prints for the orphan paths it removed (#8782).
///
/// Why: a removal that discarded unsaved work must name that discard to the
/// operator, not print a bare path.
/// What: one `removed  <path> — <reason>` line per `orphan_rows` entry; a reply
/// without rows (a daemon older than #8782) prints its bare `paths`.
/// Test: `removed_lines_name_every_discard`.
pub(crate) fn removed_lines(body: &Value) -> Vec<String> {
    if body.get("orphan_rows").is_none() {
        return body
            .get("paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
    }
    rows(body.get("orphan_rows"))
        .into_iter()
        .map(|r| format!("removed  {} — {}", r.path, r.reason))
        .collect()
}

/// The preview, grouped by project, one line per path, with a total (#8782).
///
/// What: a scope line, plus a line when the daemon does not scan this
/// checkout; then per project a header with its counts, a `remove` line per
/// path the pass would remove and an `unknown` line per path kept because its
/// pull-request state could not be read; then a total line. A reply with no
/// `scope` echo comes from a daemon older than #8782: it has no rows, so the
/// preview says so and lists that reply's orphan `paths` instead.
/// Test: `preview_lines_group_every_path_by_project_with_a_count`,
/// `preview_lines_report_an_empty_preview`,
/// `preview_lines_say_when_the_daemon_does_not_scan_this_checkout`,
/// `preview_lines_fall_back_to_the_path_list_of_an_older_daemon`.
pub(crate) fn preview_lines(body: &Value) -> Vec<String> {
    let Some(echo) = body.get("scope") else {
        return legacy_preview_lines(body);
    };
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
    if echo.get("project_known").and_then(Value::as_bool) == Some(false) {
        out.push(
            "this checkout is not registered with the daemon, so it scans none of its \
             worktrees and found nothing to prune there; pass --all-projects for every registered project (#8782)"
                .to_string(),
        );
    }
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

/// The preview for a reply from a daemon that predates #8782 (#8782).
///
/// Why: such a daemon returns no per-project rows, only the orphan `paths`
/// list, so the grouped preview would print `total: 0` over a non-empty list.
fn legacy_preview_lines(body: &Value) -> Vec<String> {
    let paths: Vec<&str> = body
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = vec![
        "prune-worktrees preview — this daemon predates #8782, so the per-project preview \
         is unavailable; restart it on the current binary for one. Orphan paths it reported:"
            .to_string(),
    ];
    out.extend(paths.iter().map(|p| format!("  remove   {p}")));
    out
}

/// Refuse a daemon reply that does not confirm the scope this run asked for
/// (#8782).
///
/// Why: the daemon is long-lived and a CLI upgrade never bounces it. A daemon
/// that predates #8782 drops `project_root` and the allowlists silently and runs
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

/// The allowlist keys a `--force` run needs the daemon to echo (#8782).
const ALLOWLIST_KEYS: [&str; 3] = [
    "only_orphan_paths",
    "only_merged_paths",
    "only_discard_paths",
];

/// Refuse a `--force` run against a daemon that does not echo every
/// allowlist, and a force reply whose allowlists differ from the plan (#8782).
///
/// Why: a daemon that drops one of these request fields runs that bound
/// unrestricted, so a preview without the keys must stop the run before the
/// removal is sent. A key's presence, even as null, says the daemon reads it.
/// What: on the preview (`planned` is `None`) every key in [`ALLOWLIST_KEYS`]
/// must be present in the `scope` echo. On the force reply each echoed size
/// must equal the length of the list sent.
/// Test: `a_force_preview_without_the_allowlist_keys_is_refused`,
/// `a_force_reply_whose_allowlist_sizes_differ_is_refused`,
/// `force_sends_nothing_after_a_preview_without_the_allowlist_keys`.
pub(crate) fn check_allowlist_echo(
    body: &Value,
    planned: Option<&PlannedPaths>,
) -> anyhow::Result<()> {
    let after = if planned.is_some() {
        "the removal has run; re-run without --force to see what remains"
    } else {
        "nothing destructive was sent; restart the daemon on the current binary and re-run"
    };
    let echo = body.get("scope");
    // In `ALLOWLIST_KEYS` order.
    let sizes = planned.map(|p| [p.orphan.len(), p.merged.len(), p.discard.len()]);
    for (i, key) in ALLOWLIST_KEYS.into_iter().enumerate() {
        let Some(echoed) = echo.and_then(|e| e.get(key)) else {
            anyhow::bail!(
                "the daemon's scope echo has no `{key}`, so it would not bound --force by this \
                 preview (#8782); {after}"
            );
        };
        if let Some(sent) = sizes.map(|s| s[i]) {
            anyhow::ensure!(
                echoed.as_u64() == u64::try_from(sent).ok(),
                "the daemon echoed `{key}` as {echoed} but {sent} path(s) were sent (#8782); {after}"
            );
        }
    }
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

#[cfg(test)]
#[path = "prune_preview_tests.rs"]
mod prune_preview_tests;
