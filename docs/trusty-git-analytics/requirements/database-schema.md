# Database Schema

`tga` uses a single SQLite database file (`tga.db` by default). WAL journal mode is
enabled on every open. All three pipeline stages (collect, classify, report) read from
and write to this single file.

```sql
-- Applied on every Database::open()
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;
```

The schema is managed by a versioned migration runner. Migrations are applied in order
at startup and are idempotent (already-applied versions are skipped).

**This page is a reference, not the source of truth.** The schema in front of you is
whatever migrations that database has actually had applied, which is not necessarily the
newest set. Read it directly:

```bash
tga inspect schema        # every table, view, and column, with free-text columns marked
tga inspect attest        # the data-handling attestation, with the live evidence for it
```

Both open the file read-only and refuse to create one, so pointing them at a missing or
unreadable path names the cause instead of reporting an empty database (issue #5218).

---

## Data-Handling Attestation

The defensible claim about this schema is:

> tga's database stores no file content, diffs, patches, hunks, or blobs.

It is **never** "contains no code". No column exists to hold a file's contents, a diff, a
patch, or a hunk, but the free-text columns below hold whatever a human or an upstream
system typed — and a pasted snippet in a commit message is stored verbatim.

| Free-text column | Source |
|---|---|
| `commits.message` | Full commit subject and body — the highest-exposure column |
| `classification_overrides.notes` | Justification typed when hand-labelling a commit |
| `pull_requests.title` | PR title from the provider |
| `work_items.title` | Ticket title from the provider |
| `work_items.tags` | Comma-separated labels from the provider |
| `linear_issues.title` | Issue title from Linear |
| `azdo_iterations.name` | Sprint name typed by whoever created the iteration |
| `work_items.raw_json` | Serialized upstream payload — a shape the writer controls, not the schema |

`tga inspect attest` reads those columns in the operator's own database rather than
citing the migration that declared them, and counts any row carrying a unified-diff
marker. The inventory itself is pinned in `src/core/inspect/text_columns.rs` and a
migration that adds an unclassified `TEXT` column fails
`core::inspect::tests::every_text_column_is_classified`.

`collect::git::diff::diff_for_commit` does compute a real unified diff (200 KiB cap,
issue #559). Its one non-test caller is the profile diff sampler
(`src/profile/diff_sampler/sampler.rs`, #5465), which holds the text in memory for the
period-review prompt and never binds it to a SQL statement. That caller list is pinned in
`src/core/inspect/attest.rs` and re-derived from the source tree by
`core::inspect::tests::diff_for_commit_callers_match_the_attestation`, so a new caller
fails the build.

---

## Tables

### `authors`

Canonical developer identities. One row per unique developer after alias resolution.
Migration `0001_initial_schema.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `canonical_name` | TEXT | no | Display name used in reports |
| `canonical_email` | TEXT | no | Primary email; UNIQUE constraint |
| `aliases` | TEXT | no | JSON array of alternate emails/handles; default `'[]'` |

**Indexes**: UNIQUE(`canonical_email`), INDEX(`canonical_email`).

---

### `commits`

Raw git commit records. One row per commit SHA. Migration `0001_initial_schema.sql`,
extended by `0003`, `0007`, `0017`, and `0021`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `sha` | TEXT | no | Full OID hex; UNIQUE constraint |
| `author_id` | INTEGER | yes | FK → `authors(id)` ON DELETE SET NULL |
| `author_name` | TEXT | no | Raw name from git log |
| `author_email` | TEXT | no | Raw email from git log |
| `timestamp` | TEXT | no | ISO 8601 UTC timestamp |
| `message` | TEXT | no | Full commit message — free text |
| `repository` | TEXT | no | Repository name (from config `name` field) |
| `files_changed` | INTEGER | no | Count of changed files; default 0 |
| `insertions` | INTEGER | no | Lines added; default 0 |
| `deletions` | INTEGER | no | Lines removed; default 0 |
| `classification_id` | INTEGER | yes | FK → `classifications(id)` ON DELETE SET NULL |
| `confidence` | REAL | yes | Classification confidence [0, 1] |
| `is_merge` | INTEGER | no | Boolean (0/1); default 0 |
| `ticketed` | INTEGER | no | Boolean — ticket reference detected; default 0 (`0003`) |
| `is_revert` | INTEGER | no | Boolean — commit is a revert; default 0 (`0007`) |
| `ticket_id` | TEXT | yes | Detected ticket reference, e.g. `ENG-123`, `AB#456` (`0007`) |
| `is_ai_assisted` | INTEGER | no | Boolean — AI co-authorship trailer detected; default 0 (`0017`) |
| `ai_tool` | TEXT | yes | `claude`, `copilot`, `cursor`, … (`0017`) |
| `agentic_mode` | TEXT | no | `none` / `ide_assisted` / `full_agentic`; default `'none'` (`0021`) |

**Indexes**: UNIQUE(`sha`), INDEX(`author_id`), INDEX(`repository`), INDEX(`timestamp`),
INDEX(`ticketed`), INDEX(`is_revert`), INDEX(`ticket_id`), INDEX(`is_ai_assisted`),
INDEX(`agentic_mode`).

There is no `complexity` column on `commits` — migration `0013` put it on
`classifications`.

---

### `classifications`

Classification results. One row per distinct verdict. Referenced by
`commits.classification_id`. Migration `0001_initial_schema.sql`, extended by `0013` and
`0017`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `category` | TEXT | no | Work category (e.g. `feature`, `bugfix`, `refactor`) |
| `subcategory` | TEXT | yes | Optional leaf label for granular reporting |
| `ticket_id` | TEXT | yes | Ticket reference extracted during classification |
| `confidence` | REAL | no | Confidence score; default 0.0 |
| `method` | TEXT | no | `exact_rule`, `regex_rule`, `fuzzy_match`, `llm_fallback`, or `manual` |
| `complexity` | INTEGER | yes | Complexity score 1–5; NULL when not scored (`0013`) |
| `top_level_category` | TEXT | yes | Category derived from the subcategory taxonomy (`0017`) |

**Indexes**: INDEX(`category`), INDEX(`top_level_category`).

---

### `files`

File-level change records. One row per (commit, file) pair. Present only when
`output.include_files: true` in config. Migration `0001_initial_schema.sql`.

Records a path and the two line counts, never the file's contents.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `commit_id` | INTEGER | no | FK → `commits(id)` ON DELETE CASCADE |
| `path` | TEXT | no | Relative file path |
| `change_type` | TEXT | no | `added`, `modified`, `deleted`, or `renamed` |
| `insertions` | INTEGER | no | Lines added; default 0 |
| `deletions` | INTEGER | no | Lines removed; default 0 |

**Indexes**: INDEX(`commit_id`), INDEX(`path`).

---

### `pull_requests`

Pull request metadata fetched from GitHub, Bitbucket, or Azure DevOps. Migration
`0001_initial_schema.sql`, extended by `0010`, `0012`, `0022`, and `0024`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `pr_number` | INTEGER | no | PR number within the repository |
| `title` | TEXT | no | PR title — free text |
| `author` | TEXT | no | Author login or display name |
| `state` | TEXT | no | `open`, `closed`, or `merged` |
| `created_at` | TEXT | no | ISO 8601 timestamp |
| `merged_at` | TEXT | yes | ISO 8601 timestamp; NULL if not merged |
| `commit_shas` | TEXT | no | JSON array of commit SHAs in the PR; default `'[]'` |
| `provider` | TEXT | no | `github`, `bitbucket`, or `azdo`; default `'github'` (`0010`) |
| `repository` | TEXT | no | Repository name; default `'unknown'` (`0012`) |
| `fetched_at` | TEXT | no | RFC3339 snapshot time, the stale-write guard; default `''` (`0022`) |
| `head_ref` | TEXT | no | Source branch name; default `''` (`0024`) |
| `body_ticket_id` | TEXT | yes | Ticket reference extracted from the PR body (`0024`) |

**Indexes**: UNIQUE(`provider`, `repository`, `pr_number`), INDEX(`pr_number`),
INDEX(`state`), INDEX(`provider`, `repository`).

`0012` replaced `0010`'s UNIQUE(`provider`, `pr_number`) index, which collapsed
cross-repository PR-number collisions (bug #88).

---

### `linear_issues`

Linear ticket data fetched on reference detection. Migration `0002_linear_issues.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `identifier` | TEXT | no | Linear issue identifier (e.g. `ENG-123`); UNIQUE constraint |
| `title` | TEXT | no | Issue title — free text |
| `state` | TEXT | no | Issue state (e.g. `In Progress`, `Done`) |
| `team` | TEXT | no | Linear team name |
| `team_key` | TEXT | no | Linear team key |
| `assignee` | TEXT | yes | Assignee display name |
| `priority` | INTEGER | no | Linear priority; default 0 |
| `url` | TEXT | no | Issue URL; default `''` |
| `fetched_at` | TEXT | no | ISO 8601 timestamp |

**Indexes**: UNIQUE(`identifier`), INDEX(`team_key`), INDEX(`state`), INDEX(`assignee`).

---

### `work_items`

Unified ticket/work-item records from JIRA, GitHub, Linear, and Azure DevOps. Migration
`0005_work_items.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | TEXT | no | PK part 1 — provider-specific ID (e.g. `ENG-123`, `42`) |
| `source` | TEXT | no | PK part 2 — `azdo`, `jira`, `github`, or `linear` |
| `title` | TEXT | no | Ticket title — free text |
| `status` | TEXT | no | Provider status string |
| `item_type` | TEXT | no | `Bug`, `User Story`, `Task`, `Epic`, … |
| `tags` | TEXT | yes | Comma-separated labels — free text |
| `project` | TEXT | yes | Project name or key |
| `url` | TEXT | yes | Ticket URL |
| `raw_json` | TEXT | yes | Full JSON payload as the collector serialized it |
| `fetched_at` | TEXT | no | Default `datetime('now')` |

**PK**: (`id`, `source`) — an ADO `42` and a JIRA `42` are distinct rows.

The composite `(id, source)` key replaced the `(provider, external_id)` shape earlier
drafts of this page described; `provider`, `external_id`, `work_item_type`, and `state`
have never existed in the shipped schema.

`raw_json` is the one column whose contract does not constrain what a future writer puts
in it — today's writer serializes a struct with no description field. `tga inspect attest`
therefore reads the column rather than citing this table.

---

### `commit_work_items`

Many-to-many join between commits and work items. Migration `0005_work_items.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `commit_sha` | TEXT | no | Commit SHA (the natural key, not `commits.id`) |
| `work_item_id` | TEXT | no | FK part 1 → `work_items(id)` |
| `work_item_source` | TEXT | no | FK part 2 → `work_items(source)` |

**PK**: (`commit_sha`, `work_item_id`, `work_item_source`).
**Indexes**: INDEX(`commit_sha`), INDEX(`work_item_id`, `work_item_source`).

---

### `classification_overrides`

Manual Tier 0 overrides. Entries here take absolute priority over all rule-based and
LLM classifications. Managed via `tga override add|list|remove`. Migration
`0006_classification_overrides.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `commit_sha` | TEXT | no | PK part 1 — commit SHA |
| `repo_path` | TEXT | no | PK part 2 — repository scope, so a fork can differ |
| `work_type` | TEXT | no | Subcategory (e.g. `feature`, `bugfix`) |
| `change_type` | TEXT | no | Top-level category |
| `notes` | TEXT | yes | Justification — free text |
| `created_at` | TEXT | no | Default `datetime('now')` |

**PK**: (`commit_sha`, `repo_path`). **Indexes**: INDEX(`commit_sha`).

---

### `repository_analysis_status`

Per-repository classification-coverage bookkeeping. One row per repository name.
Migration `0006_classification_overrides.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `repo_name` | TEXT PK | no | Repository name |
| `last_analyzed_at` | TEXT | no | Default `datetime('now')` |
| `classification_coverage_pct` | REAL | yes | `classified / total * 100` |
| `total_commits` | INTEGER | no | Total commits in the DB for this repo; default 0 |
| `classified_commits` | INTEGER | no | Commits with a non-`uncategorized` verdict; default 0 |

---

### `collection_runs`

Per-(repo, ISO year, ISO week) collection bookkeeping. Presence of a row signals that
this tuple has already been collected; `--force` bypasses the check. Migrations
`0004_collection_runs.sql` and `0009_collection_runs_repo_count.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `repo_name` | TEXT | no | Repository name |
| `iso_year` | INTEGER | no | ISO year (e.g. 2026) |
| `iso_week` | INTEGER | no | ISO week number 1–53 |
| `collected_at` | TEXT | no | ISO 8601 timestamp |
| `commit_count` | INTEGER | no | Commits collected; default 0 |
| `repo_count` | INTEGER | no | Size of `repositories[]` at write time; default 0 (`0009`) |

**Indexes**: UNIQUE(`repo_name`, `iso_year`, `iso_week`),
INDEX(`repo_name`, `iso_year`, `iso_week`).

`repo_count` enables week-over-week baseline drift detection when repository lists change.

---

### `repo_walk_state`

Per-repository full-history walk bookkeeping (issue #6073), so an incremental collection
knows which tips it has already walked. Migration `0025_repo_walk_state.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `repository` | TEXT PK | no | Repository name |
| `head_sha` | TEXT | no | HEAD at the last walk |
| `head_ref` | TEXT | no | HEAD's symbolic ref at the last walk |
| `tips_digest` | TEXT | no | Digest over the tip set the walk covered |
| `walk_scope` | TEXT | no | Scope label for that walk; default `''` |
| `walk_complete` | INTEGER | no | Boolean — the walk finished; default 0 |
| `walked_at` | TEXT | no | Timestamp of the walk |

---

### `azdo_iterations`

Azure DevOps iteration/sprint data. Migration `0008_azdo_iterations.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | TEXT PK | no | ADO iteration GUID |
| `project` | TEXT | no | ADO project name |
| `name` | TEXT | no | Iteration display name — free text |
| `path` | TEXT | yes | Iteration path |
| `start_date` | TEXT | yes | ISO 8601 date |
| `finish_date` | TEXT | yes | ISO 8601 date |
| `time_frame` | TEXT | yes | `past`, `current`, or `future` |
| `fetched_at` | TEXT | no | Default `datetime('now')` |

**Indexes**: INDEX(`project`).

---

### `pr_reviewers`

Per-PR reviewer records. Migrations `0011_pr_reviewers.sql` and
`0020_pr_reviewers_review_state.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `pr_id` | INTEGER | no | FK → `pull_requests(id)` ON DELETE CASCADE |
| `provider` | TEXT | no | `azdo` or `github`; default `'azdo'` |
| `reviewer_id` | TEXT | no | Upstream identity ID |
| `display_name` | TEXT | yes | Human-readable name |
| `vote` | INTEGER | no | ADO vote: 10 approved, 5 approved-with-suggestions, 0 no-vote, -5 waiting, -10 rejected; default 0 |
| `is_required` | BOOLEAN | no | Whether reviewer approval is required; default 0 |
| `is_container` | BOOLEAN | no | Whether the entry is a group/team; default 0 |
| `review_state` | TEXT | yes | GitHub review state: `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED` (`0020`) |
| `submitted_at` | TEXT | yes | GitHub review timestamp (`0020`) |

**Indexes**: UNIQUE(`pr_id`, `provider`, `reviewer_id`), INDEX(`pr_id`).

---

### `fact_deployments`

Canonical DORA deploy-event hub; every DORA query joins through it. Migration
`0014_dora_tables.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `deploy_id` | TEXT PK | no | Upstream primary key (Actions run id, release tag) |
| `repo` | TEXT | no | Repository name |
| `environment` | TEXT | no | Default `'production'` |
| `triggered_at` | TIMESTAMP | yes | |
| `completed_at` | TIMESTAMP | yes | |
| `status` | TEXT | yes | e.g. `success` |
| `git_sha` | TEXT | yes | Deployed commit |
| `git_tag` | TEXT | yes | Deployed tag |
| `triggered_by_pr` | INTEGER | yes | PR number that triggered the deploy |
| `source` | TEXT | yes | Ingestion source |

**Indexes**: INDEX(`repo`), INDEX(`triggered_at`), INDEX(`environment`, `status`),
INDEX(`git_sha`).

---

### `fact_incidents`

Production-incident observations. `mttr_hours` is denormalised on write so DORA
aggregation stays cheap. Migration `0014_dora_tables.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `incident_id` | TEXT PK | no | Upstream incident ID |
| `source` | TEXT | yes | Datadog, PagerDuty, JIRA SRE, … |
| `detected_at` | TIMESTAMP | yes | |
| `resolved_at` | TIMESTAMP | yes | |
| `mttr_hours` | REAL | yes | Denormalised recovery time |
| `severity` | TEXT | yes | |
| `triggering_deploy` | TEXT | yes | FK → `fact_deployments(deploy_id)` ON DELETE SET NULL |
| `repo` | TEXT | yes | |
| `jira_ticket` | TEXT | yes | |

**Indexes**: INDEX(`repo`), INDEX(`detected_at`), INDEX(`source`).

---

### `deployment_failures`

Derived join between a deployment and the commits that caused and fixed a failure.
Populated by the `tga dora` analysis pass. Migration `0014_dora_tables.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `deploy_id` | TEXT | yes | FK → `fact_deployments(deploy_id)` ON DELETE SET NULL |
| `failure_commit_sha` | TEXT | yes | |
| `recovery_commit_sha` | TEXT | yes | |
| `detected_at` | TIMESTAMP | yes | |
| `recovered_at` | TIMESTAMP | yes | |

**Indexes**: INDEX(`deploy_id`), INDEX(`failure_commit_sha`).

---

### `fact_commit_reachability`

Whether a commit is reachable from the default branch, any tag, or a release branch
(issue #279). A separate fact table rather than columns on `commits`, so the reachability
pass can re-run independently of collection. Migration
`0015_tag_release_branch_reachability.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `commit_sha` | TEXT PK | no | FK → `commits(sha)` ON DELETE CASCADE |
| `on_default_branch` | INTEGER | no | Boolean; default 0 |
| `on_any_tag` | INTEGER | no | Boolean; default 0 |
| `reachable_from_tags` | TEXT | no | JSON array of tag names; default `'[]'` |
| `on_release_branch` | INTEGER | no | Boolean; default 0 |
| `release_branches` | TEXT | no | JSON array of branch names; default `'[]'` |

**Indexes**: INDEX(`on_any_tag`), INDEX(`on_release_branch`), INDEX(`on_default_branch`).

---

### `fact_commit_effort`

Per-commit empirical effort scores, written by `tga backfill effort`. Migrations
`0016_fact_commit_effort.sql` and `0017_pushdown_445.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `sha` | TEXT | no | PK part 1 |
| `repository` | TEXT | no | PK part 2 — the same SHA can appear in a fork or mirror |
| `size` | TEXT | no | `XS` / `S` / `M` / `L` / `XL`, from absolute score thresholds |
| `score` | REAL | no | v1 formula: α·log₂(LoC+1) + β·log₂(files+1) + δ·tests_factor |
| `loc` | INTEGER | no | Insertions + deletions |
| `files` | INTEGER | no | |
| `test_loc` | INTEGER | no | |
| `tests_factor` | REAL | no | |
| `formula_version` | TEXT | no | Default `'v1'` |
| `computed_at` | INTEGER | no | Unix timestamp (seconds) |
| `effort_tshirt` | INTEGER | yes | Corpus-percentile band 1–5 (`0017`) |

**PK**: (`sha`, `repository`).
**Indexes**: INDEX(`size`), INDEX(`repository`), INDEX(`score`), INDEX(`effort_tshirt`).

`size` and `effort_tshirt` diverge on purpose: `size` bands the absolute score, while
`effort_tshirt` ranks the commit against the corpus percentiles in
`effort_percentile_thresholds`.

---

### `effort_percentile_thresholds`

The p20/p40/p60/p80 breakpoints an incremental insert bins against, so single-commit
ingestion does not re-scan the corpus. Migration `0019_effort_percentile_stats.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `dataset` | TEXT PK | no | `'default'` for the main corpus |
| `p20` | REAL | no | |
| `p40` | REAL | no | |
| `p60` | REAL | no | |
| `p80` | REAL | no | |
| `sample_count` | INTEGER | no | Corpus size the breakpoints were computed over |
| `computed_at` | INTEGER | no | Unix timestamp (seconds) |

---

### `fact_weekly_quality`

Per-engineer-per-week quality scores, UPSERTed by the report aggregator so downstream
warehouses can join on them without re-running it. Migration
`0018_fact_weekly_quality.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `author_email` | TEXT | no | PK part 1 — canonical email |
| `iso_year` | INTEGER | no | PK part 2 |
| `iso_week` | INTEGER | no | PK part 3 (1–53) |
| `repository` | TEXT | no | PK part 4 |
| `quality_score` | REAL | no | [0.0, 1.0]; 0.35·(1−revert_rate) + 0.40·(1−bugfix_rate) + 0.25·ticket_rate |
| `quality_tshirt` | INTEGER | no | Band 1–5 (5 = best) |
| `revert_count` | INTEGER | no | Default 0 |
| `bugfix_count` | INTEGER | no | Default 0 |
| `ticketed_count` | INTEGER | no | Default 0 |
| `commit_count` | INTEGER | no | Default 0 |
| `formula_version` | TEXT | no | Default `'v1'` |
| `computed_at` | INTEGER | no | Unix timestamp (seconds); default 0 |

**PK**: (`author_email`, `iso_year`, `iso_week`, `repository`).
**Indexes**: INDEX(`iso_year`, `iso_week`), INDEX(`author_email`), INDEX(`repository`).

---

### `fact_weekly_engineer`

Per-engineer-per-week agentic-authorship roll-up (issue #1113). Created by migration 21.
`0021_agentic_mode.sql` is a reference copy the runner does not execute — the live
statements are in `src/core/db/migrations/v21.rs`, which guards `commits.agentic_mode`
with a `PRAGMA table_info` check because SQLite has no `ADD COLUMN IF NOT EXISTS`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `author_email` | TEXT | no | PK part 1 |
| `iso_year` | INTEGER | no | PK part 2 |
| `iso_week` | INTEGER | no | PK part 3 |
| `repository` | TEXT | no | PK part 4 |
| `net_commits` | INTEGER | no | Default 0 |
| `agentic_count` | INTEGER | no | Commits with `agentic_mode = 'full_agentic'`; default 0 |
| `ide_assisted_count` | INTEGER | no | Commits with `agentic_mode = 'ide_assisted'`; default 0 |
| `agentic_pct` | REAL | no | Default 0.0 |
| `formula_version` | TEXT | no | Default `'v1'` |
| `computed_at` | INTEGER | no | Unix timestamp (seconds); default 0 |

**PK**: (`author_email`, `iso_year`, `iso_week`, `repository`).
**Indexes**: INDEX(`iso_year`, `iso_week`), INDEX(`author_email`), INDEX(`repository`).

---

### `fact_ticket_transitions`

One row per JIRA changelog status transition, written by `tga jira sync` (issue #3966).
Migration `0023_jira_ingestion.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `ticket_key` | TEXT | no | PK part 1 |
| `project_key` | TEXT | no | |
| `from_status` | TEXT | yes | NULL for a ticket's initial creation state |
| `to_status` | TEXT | no | PK part 3 |
| `transitioned_at` | TEXT | no | PK part 2 — RFC3339 |
| `author` | TEXT | yes | NULL when JIRA reports no changelog author |
| `synced_at` | INTEGER | no | Unix seconds when tga wrote the row |

**PK**: (`ticket_key`, `transitioned_at`, `to_status`).
**Indexes**: INDEX(`project_key`), INDEX(`synced_at`), INDEX(`ticket_key`).

---

### `fact_jira_comment_detail`

One row per JIRA comment. Stores the comment's LENGTH, never its text. Migration
`0023_jira_ingestion.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `ticket_key` | TEXT | no | PK part 1 |
| `comment_id` | TEXT | no | PK part 2 |
| `project_key` | TEXT | no | |
| `author` | TEXT | yes | NULL when JIRA reports no comment author |
| `created_at` | TEXT | no | RFC3339 |
| `body_len` | INTEGER | no | Length of the comment body; see `collect::jira::client` for the ADF-vs-plain-text caveat |
| `synced_at` | INTEGER | no | Unix seconds when tga wrote the row |

**PK**: (`ticket_key`, `comment_id`).
**Indexes**: INDEX(`project_key`), INDEX(`synced_at`).

---

### `jira_sync_cursor`

Per-project incremental-sync cursor for `tga jira sync`. Migration
`0023_jira_ingestion.sql`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `project_key` | TEXT PK | no | |
| `last_synced_at` | TEXT | no | RFC3339 — the JQL `updated >=` bound for the next run |
| `last_run_at` | TEXT | no | RFC3339 — wall clock of the last successful sync |
| `tickets_synced` | INTEGER | no | Default 0 |

---

### `fact_pm_work`

PM ticket meaningfulness verdicts — the WORK tier of the Activity / Work / Effort model,
filtering raw ticket activity down to the subset that represents real management labor
(issue #3916). Populated by `tga backfill pm-work`. Migration `0026_fact_pm_work.sql`.

Rows here are counted in tickets; `fact_commit_effort` rows are counted in commit
effort points. The two are incommensurable and must never share a visualization
axis (issue #3917).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `work_item_id` | TEXT | no | FK → `work_items(id)` |
| `work_item_source` | TEXT | no | FK → `work_items(source)`; `jira`, `github`, `azdo`, `linear` |
| `pm_name` | TEXT | yes | Reporter display name from the source payload |
| `week_key` | TEXT | yes | ISO week the ticket was created, `YYYY-Www` |
| `is_meaningful` | INTEGER | no | `0` or `1` |
| `exclusion_reason` | TEXT | no | `NONE`, `TERSE_TITLE`, `AUTO_GENERATED`, or `BOT_FILED` |
| `title_word_count` | INTEGER | no | |
| `body_word_count` | INTEGER | no | Words in the extracted description body |
| `formula_version` | TEXT | no | Threshold set that produced the verdict; `pm-work-1` for v1 |
| `computed_at` | INTEGER | no | Unix timestamp (seconds) |

**PK**: (`work_item_id`, `work_item_source`) — UPSERT semantics, so re-running the
classifier replaces a verdict rather than accumulating one row per version.

**Indexes**: INDEX(`is_meaningful`), INDEX(`exclusion_reason`), INDEX(`pm_name`, `week_key`).

---

### `fact_pm_effort`

PM effort tier (issue #3915): one row per meaningful PM ticket, carrying the
complexity score that producing it required. Written by `tga backfill pm-effort`
from `src/core/pm_effort/`. Migration `0027_fact_pm_effort.sql`.

Only tickets `fact_pm_work` marks `is_meaningful = 1` are scored — an excluded
ticket gets no row at all, and the backfill deletes a row whose ticket has since
stopped being meaningful. `tga backfill pm-work` must therefore run first.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `work_item_id` | TEXT | no | PK part 1; FK → `work_items(id)` |
| `work_item_source` | TEXT | no | PK part 2; FK → `work_items(source)`. `jira`, `azdo`, `github`, `linear` |
| `pm_name` | TEXT | yes | Reporter display name from the upstream payload |
| `week_key` | TEXT | yes | ISO week the ticket was created, `YYYY-Www` |
| `effort_score` | REAL | yes | 1.0–50.0; NULL when `score_status <> 'SCORED'` |
| `effort_bucket` | TEXT | yes | `LOW` (<15) / `MEDIUM` (15–30) / `HIGH` (≥30); NULL alongside a NULL score |
| `score_status` | TEXT | no | `SCORED` or `DEFERRED_RECENT` |
| `epic_children_count` | INTEGER | no | `work_items` rows in the same source naming this ticket as parent |
| `description_word_count` | INTEGER | no | Words of plain-text description |
| `comment_count` | INTEGER | no | `fact_jira_comment_detail` rows for this ticket |
| `transition_count` | INTEGER | no | `fact_ticket_transitions` rows for this ticket |
| `story_points` | REAL | yes | NULL when absent, unparseable, or outside 0.5–40 |
| `inputs_present` | TEXT | no | Comma-separated contributing terms, or `NONE` |
| `age_days_at_score` | INTEGER | yes | Ticket age in days when scored; NULL when the creation date is unknown |
| `formula_version` | TEXT | no | Weight set that produced the score; `pm-effort-1` for v1 |
| `computed_at` | INTEGER | no | Unix timestamp (seconds) of the write |

**Indexes**: INDEX(`effort_bucket`), INDEX(`score_status`),
INDEX(`pm_name`, `week_key`).

**Recency floor.** A ticket of a decomposable type (epic, feature, initiative)
younger than 7 days stores `score_status = 'DEFERRED_RECENT'` with NULL score
and NULL bucket. A zero would read as "no complexity"; the correct record is
"too early to tell", because that ticket's child count grows only as the team
breaks it down. A bug or task has no such input, so the floor does not apply
to it.

**Story points degrade the score rather than zeroing it.** The field is 76%
NULL across four per-project custom-field IDs on the source JIRA instance, so
its term is simply dropped when absent and the other four still produce a
score. `inputs_present` names which terms fired, so a consumer can compare
like with like.

**Scale separation.** `effort_score` here is counted in PM complexity points
and `fact_commit_effort.effort_score` in commit effort points. The overlapping
numeric ranges (1–50 and 2.5–45) are a coincidence of scaling, not a shared
unit — the two must never share a visualization axis. See #3917.

---

### `schema_migrations`

Migration bookkeeping, created by the runner itself before the first migration
(`src/core/db/migrations/mod.rs`, `ensure_migrations_table`).

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `version` | INTEGER PK | no | Migration version |
| `name` | TEXT | no | Migration label |
| `applied_at` | TEXT | no | RFC3339 |

---

## Views

All four are thin wrappers over `fact_deployments` / `fact_incidents`, added by
`0014_dora_tables.sql` so reports do not re-write the same aggregation three times.

| View | What it returns |
|---|---|
| `v_deployment_frequency` | Weekly count of successful production deploys per repo |
| `v_lead_time` | Hours between a commit's author date and the production deploy of that SHA |
| `v_mttr` | Incident detection-to-resolution hours, from `fact_incidents.mttr_hours` |
| `v_change_failure_rate` | Per-repo weekly failed/total deploy ratio |

---

## Migration History

Migrations live in `src/core/db/sql/` and are registered in
`src/core/db/migrations/mod.rs`. Never modify an applied migration — always add a new one.

| Version | File | Description |
|---|---|---|
| 1 | `0001_initial_schema.sql` | `authors`, `commits`, `classifications`, `files`, `pull_requests` |
| 2 | `0002_linear_issues.sql` | `linear_issues` table |
| 3 | `0003_commits_ticketed.sql` | `ticketed` column on `commits` |
| 4 | `0004_collection_runs.sql` | `collection_runs` table for per-week bookkeeping |
| 5 | `0005_work_items.sql` | `work_items` and `commit_work_items` tables |
| 6 | `0006_classification_overrides.sql` | `classification_overrides` and `repository_analysis_status` tables |
| 7 | `0007_pr_metrics_and_backfill.sql` | `is_revert` and `ticket_id` columns on `commits` |
| 8 | `0008_azdo_iterations.sql` | `azdo_iterations` table |
| 9 | `0009_collection_runs_repo_count.sql` | `repo_count` column on `collection_runs` |
| 10 | `0010_pull_requests_provider.sql` | `provider` column and UNIQUE(`provider`, `pr_number`) on `pull_requests` |
| 11 | `0011_pr_reviewers.sql` | `pr_reviewers` table |
| 12 | `0012_pull_requests_repository.sql` | `repository` column on `pull_requests`; UNIQUE index rebuilt as (`provider`, `repository`, `pr_number`) |
| 13 | `0013_complexity.sql` | `complexity` column on `classifications` |
| 14 | `0014_dora_tables.sql` | `fact_deployments`, `fact_incidents`, `deployment_failures`, and the four DORA views |
| 15 | `0015_tag_release_branch_reachability.sql` | `fact_commit_reachability` table |
| 16 | `0016_fact_commit_effort.sql` | `fact_commit_effort` table |
| 17 | `0017_pushdown_445.sql` | `classifications.top_level_category`, `fact_commit_effort.effort_tshirt`, `commits.is_ai_assisted` / `commits.ai_tool`; applied through `migrations/v17.rs` for its column guard |
| 18 | `0018_fact_weekly_quality.sql` | `fact_weekly_quality` table |
| 19 | `0019_effort_percentile_stats.sql` | `effort_percentile_thresholds` table |
| 20 | `0020_pr_reviewers_review_state.sql` | `review_state` and `submitted_at` columns on `pr_reviewers` |
| 21 | `0021_agentic_mode.sql` | `commits.agentic_mode` and the `fact_weekly_engineer` table; applied through `migrations/v21.rs` for its column guard |
| 22 | `0022_pull_requests_fetched_at.sql` | `fetched_at` column on `pull_requests` — the stale-write guard |
| 23 | `0023_jira_ingestion.sql` | `fact_ticket_transitions`, `fact_jira_comment_detail`, `jira_sync_cursor` tables |
| 24 | `0024_pull_requests_head_ref_and_body_ticket.sql` | `head_ref` and `body_ticket_id` columns on `pull_requests` |
| 25 | `0025_repo_walk_state.sql` | `repo_walk_state` table |
| 26 | `0026_fact_pm_work.sql` | `fact_pm_work` table |
| 27 | `0027_fact_pm_effort.sql` | `fact_pm_effort` table |

Migrations 17 and 21 carry a `PRAGMA table_info` pre-flight guard, because a pre-release
build may already have added their columns and SQLite has no
`ALTER TABLE … ADD COLUMN IF NOT EXISTS`. `migrations/mod.rs` routes both to a Rust
module — `v17.rs` and `v21.rs` — so their `.sql` files are read as reference rather than
executed. Migration 21's registry entry carries an empty `sql` string to make that
explicit.

Future migrations continue from `0028_*.sql`.
