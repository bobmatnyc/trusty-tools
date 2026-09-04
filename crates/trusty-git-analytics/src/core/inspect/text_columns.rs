//! The pinned classification of every declared-`TEXT` column in tga's schema.
//!
//! Why: #5218 — "no file content, diffs, patches, hunks, or blobs" is a claim
//! about column TYPES, and it stays true while a free-text column quietly
//! accumulates whatever a human pasted into a commit message or a ticket
//! title. A reviewer needs those columns named, and the naming has to survive
//! the next migration: an inventory written once goes stale silently, so
//! `tests::every_text_column_is_classified` fails the build when a migration
//! adds a `TEXT` column nobody put in one of these three lists.
//! What: [`FREE_TEXT`], [`EMBEDDED_PAYLOAD`], and [`CONSTRAINED`] partition the
//! schema's `TEXT` columns; [`classify`] resolves one `table.column` against
//! them.
//! Test: `core::inspect::tests::every_text_column_is_classified`,
//! `core::inspect::tests::text_class_lists_are_disjoint`.

use serde::Serialize;

/// How constrained the text in a `TEXT` column is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextClass {
    /// Unconstrained prose a human or an upstream system typed. Nothing in the
    /// schema stops a pasted code snippet from landing here, so `tga inspect
    /// attest` scans these columns at runtime rather than reasoning from the
    /// migration files.
    FreeText,
    /// A serialized upstream payload. Today's writer puts a known, snippet-free
    /// shape in it, but the column's declared type does not constrain a future
    /// writer, so it is scanned at runtime alongside the free-text columns.
    EmbeddedPayload,
    /// An identifier, enumerated value, timestamp, or JSON array of
    /// identifiers — a shape the writer controls end to end.
    Constrained,
    /// A `TEXT` column none of the three lists names. Reported rather than
    /// assumed harmless; the coverage test fails on one in tga's own schema.
    Unclassified,
}

impl TextClass {
    /// Whether `tga inspect attest` scans this column's live contents.
    ///
    /// An unclassified column is scanned too — the safe reading of "nobody has
    /// said what goes in here" is that anything might.
    pub fn is_scanned(self) -> bool {
        !matches!(self, TextClass::Constrained)
    }
}

/// Free-text columns — the inventory a reviewer is owed by name.
///
/// `commits.message` is the highest-exposure entry: it holds the full commit
/// subject and body, which is where a pasted stack trace or code fragment
/// realistically ends up.
pub const FREE_TEXT: &[&str] = &[
    // Sprint/iteration display name, typed by whoever created the iteration.
    "azdo_iterations.name",
    // Justification a human typed when hand-labelling a commit.
    "classification_overrides.notes",
    // Full commit subject + body.
    "commits.message",
    "linear_issues.title",
    "pull_requests.title",
    // Comma-separated labels, typed upstream.
    "work_items.tags",
    "work_items.title",
];

/// Serialized upstream payloads.
///
/// `work_items.raw_json` is the whole list. Its only writer today serializes a
/// struct with no description field, so it is clean in practice — but that is a
/// property of the writer, not of the column, which is why the attestation
/// reads the column instead of citing `0005_work_items.sql`.
pub const EMBEDDED_PAYLOAD: &[&str] = &["work_items.raw_json"];

/// Every remaining `TEXT` column: identifiers, enumerated values, timestamps,
/// and JSON arrays of identifiers.
pub const CONSTRAINED: &[&str] = &[
    "authors.canonical_name",
    "authors.canonical_email",
    "authors.aliases",
    "azdo_iterations.id",
    "azdo_iterations.project",
    "azdo_iterations.path",
    "azdo_iterations.start_date",
    "azdo_iterations.finish_date",
    "azdo_iterations.time_frame",
    "azdo_iterations.fetched_at",
    "classification_overrides.commit_sha",
    "classification_overrides.repo_path",
    "classification_overrides.work_type",
    "classification_overrides.change_type",
    "classification_overrides.created_at",
    "classifications.category",
    "classifications.subcategory",
    "classifications.ticket_id",
    "classifications.method",
    "classifications.top_level_category",
    "collection_runs.repo_name",
    "collection_runs.collected_at",
    "commit_work_items.commit_sha",
    "commit_work_items.work_item_id",
    "commit_work_items.work_item_source",
    "commits.sha",
    "commits.author_name",
    "commits.author_email",
    "commits.timestamp",
    "commits.repository",
    "commits.ticket_id",
    "commits.ai_tool",
    "commits.agentic_mode",
    "deployment_failures.deploy_id",
    "deployment_failures.failure_commit_sha",
    "deployment_failures.recovery_commit_sha",
    "effort_percentile_thresholds.dataset",
    "fact_commit_effort.sha",
    "fact_commit_effort.repository",
    "fact_commit_effort.size",
    "fact_commit_effort.formula_version",
    "fact_commit_reachability.commit_sha",
    "fact_commit_reachability.reachable_from_tags",
    "fact_commit_reachability.release_branches",
    "fact_deployments.deploy_id",
    "fact_deployments.repo",
    "fact_deployments.environment",
    "fact_deployments.status",
    "fact_deployments.git_sha",
    "fact_deployments.git_tag",
    "fact_deployments.source",
    "fact_incidents.incident_id",
    "fact_incidents.source",
    "fact_incidents.severity",
    "fact_incidents.triggering_deploy",
    "fact_incidents.repo",
    "fact_incidents.jira_ticket",
    "fact_jira_comment_detail.ticket_key",
    "fact_jira_comment_detail.comment_id",
    "fact_jira_comment_detail.project_key",
    "fact_jira_comment_detail.author",
    "fact_jira_comment_detail.created_at",
    "fact_pm_effort.work_item_id",
    "fact_pm_effort.work_item_source",
    "fact_pm_effort.pm_name",
    "fact_pm_effort.week_key",
    "fact_pm_effort.effort_bucket",
    "fact_pm_effort.score_status",
    "fact_pm_effort.inputs_present",
    "fact_pm_effort.formula_version",
    "fact_pm_work.work_item_id",
    "fact_pm_work.work_item_source",
    "fact_pm_work.pm_name",
    "fact_pm_work.week_key",
    "fact_pm_work.exclusion_reason",
    "fact_pm_work.formula_version",
    "fact_ticket_transitions.ticket_key",
    "fact_ticket_transitions.project_key",
    "fact_ticket_transitions.from_status",
    "fact_ticket_transitions.to_status",
    "fact_ticket_transitions.transitioned_at",
    "fact_ticket_transitions.author",
    "fact_weekly_engineer.author_email",
    "fact_weekly_engineer.repository",
    "fact_weekly_engineer.formula_version",
    "fact_weekly_quality.author_email",
    "fact_weekly_quality.repository",
    "fact_weekly_quality.formula_version",
    "files.path",
    "files.change_type",
    "jira_sync_cursor.project_key",
    "jira_sync_cursor.last_synced_at",
    "jira_sync_cursor.last_run_at",
    "linear_issues.identifier",
    "linear_issues.state",
    "linear_issues.team",
    "linear_issues.team_key",
    "linear_issues.assignee",
    "linear_issues.url",
    "linear_issues.fetched_at",
    "pr_reviewers.provider",
    "pr_reviewers.reviewer_id",
    "pr_reviewers.display_name",
    "pr_reviewers.review_state",
    "pr_reviewers.submitted_at",
    "pull_requests.author",
    "pull_requests.state",
    "pull_requests.created_at",
    "pull_requests.merged_at",
    "pull_requests.commit_shas",
    "pull_requests.provider",
    "pull_requests.repository",
    "pull_requests.fetched_at",
    "pull_requests.head_ref",
    "pull_requests.body_ticket_id",
    "repo_walk_state.repository",
    "repo_walk_state.head_sha",
    "repo_walk_state.head_ref",
    "repo_walk_state.tips_digest",
    "repo_walk_state.walk_scope",
    "repo_walk_state.walked_at",
    "repository_analysis_status.repo_name",
    "repository_analysis_status.last_analyzed_at",
    "schema_migrations.name",
    "schema_migrations.applied_at",
    "work_items.id",
    "work_items.source",
    "work_items.status",
    "work_items.item_type",
    "work_items.project",
    "work_items.url",
    "work_items.fetched_at",
];

/// Resolve one `table.column` against the three inventories.
///
/// Why: the schema reader and the attestation must agree on a column's class,
/// and a second copy of the lookup is a second thing to keep in step.
/// What: linear scan of [`FREE_TEXT`], then [`EMBEDDED_PAYLOAD`], then
/// [`CONSTRAINED`]; anything else is [`TextClass::Unclassified`]. The lists
/// hold ~140 entries and a caller runs this once per column, so a scan costs
/// less than building a map.
/// Test: `core::inspect::tests::every_text_column_is_classified`.
pub fn classify(table: &str, column: &str) -> TextClass {
    let key = format!("{table}.{column}");
    if FREE_TEXT.contains(&key.as_str()) {
        TextClass::FreeText
    } else if EMBEDDED_PAYLOAD.contains(&key.as_str()) {
        TextClass::EmbeddedPayload
    } else if CONSTRAINED.contains(&key.as_str()) {
        TextClass::Constrained
    } else {
        TextClass::Unclassified
    }
}
