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

---

## Tables

### `authors`

Canonical developer identities. One row per unique developer after alias resolution.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `canonical_name` | TEXT | no | Display name used in reports |
| `canonical_email` | TEXT | no | Primary email; UNIQUE constraint |
| `aliases` | TEXT | no | JSON array of alternate emails/handles; default `'[]'` |

**Indexes**: UNIQUE(`canonical_email`).

---

### `commits`

Raw git commit records. One row per commit SHA.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `sha` | TEXT | no | Full OID hex; UNIQUE constraint |
| `author_id` | INTEGER | yes | FK → `authors(id)` ON DELETE SET NULL |
| `author_name` | TEXT | no | Raw name from git log |
| `author_email` | TEXT | no | Raw email from git log |
| `timestamp` | TEXT | no | ISO 8601 UTC timestamp |
| `message` | TEXT | no | Full commit message |
| `repository` | TEXT | no | Repository name (from config `name` field) |
| `files_changed` | INTEGER | no | Count of changed files; default 0 |
| `insertions` | INTEGER | no | Lines added; default 0 |
| `deletions` | INTEGER | no | Lines removed; default 0 |
| `classification_id` | INTEGER | yes | FK → `classifications(id)` ON DELETE SET NULL |
| `confidence` | REAL | yes | Classification confidence [0, 1] |
| `is_merge` | INTEGER | no | Boolean (0/1); default 0 |
| `ticketed` | INTEGER | no | Boolean — ticket reference detected; default 0 |
| `ticket_id` | TEXT | yes | Detected ticket reference (e.g. `ENG-123`, `AB#456`) |
| `is_revert` | INTEGER | no | Boolean — commit is a revert; default 0 |
| `complexity` | INTEGER | yes | Complexity score 1–5 (populated by `--backfill-complexity`) |

**Indexes**: UNIQUE(`sha`), INDEX(`author_id`), INDEX(`repository`), INDEX(`timestamp`).

---

### `classifications`

Classification results. One row per distinct verdict. Referenced by `commits.classification_id`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `category` | TEXT | no | Work category (e.g. `feature`, `bugfix`, `refactor`) |
| `subcategory` | TEXT | yes | Optional leaf label for granular reporting |
| `ticket_id` | TEXT | yes | Ticket reference extracted during classification |
| `confidence` | REAL | no | Confidence score; default 0.0 |
| `method` | TEXT | no | `exact_rule`, `regex_rule`, `fuzzy_match`, `llm_fallback`, or `manual` |

---

### `files`

File-level change records. One row per (commit, file) pair. Present only when
`output.include_files: true` in config.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `commit_id` | INTEGER | no | FK → `commits(id)` ON DELETE CASCADE |
| `path` | TEXT | no | Relative file path |
| `change_type` | TEXT | no | `added`, `modified`, `deleted`, or `renamed` |
| `insertions` | INTEGER | no | Lines added; default 0 |
| `deletions` | INTEGER | no | Lines removed; default 0 |

**Indexes**: INDEX(`commit_id`).

---

### `pull_requests`

Pull request metadata fetched from GitHub, Bitbucket, or Azure DevOps.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `provider` | TEXT | no | `github`, `bitbucket`, or `azdo`; default `github` |
| `pr_number` | INTEGER | no | PR number within the repository |
| `title` | TEXT | no | PR title |
| `author` | TEXT | no | Author login or display name |
| `state` | TEXT | no | `open`, `closed`, or `merged` |
| `created_at` | TEXT | no | ISO 8601 timestamp |
| `merged_at` | TEXT | yes | ISO 8601 timestamp; NULL if not merged |
| `commit_shas` | TEXT | no | JSON array of commit SHAs in the PR; default `'[]'` |
| `merge_commit_sha` | TEXT | yes | SHA of the merge commit (when available) |

**Indexes**: UNIQUE(`provider`, `pr_number`).

---

### `linear_issues`

Linear ticket data fetched on reference detection.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `issue_id` | TEXT | no | Linear issue identifier (e.g. `ENG-123`) |
| `title` | TEXT | yes | Issue title |
| `state` | TEXT | yes | Issue state (e.g. `In Progress`, `Done`) |
| `team_key` | TEXT | yes | Linear team key |
| `url` | TEXT | yes | Issue URL |
| `fetched_at` | TEXT | no | ISO 8601 timestamp |

**Indexes**: UNIQUE(`issue_id`).

---

### `work_items`

Unified ticket/work-item records from JIRA, Linear, and Azure DevOps.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `provider` | TEXT | no | `jira`, `linear`, or `azdo` |
| `external_id` | TEXT | no | Provider-specific ID (e.g. `ENG-123`, `AB#456`) |
| `title` | TEXT | yes | |
| `work_item_type` | TEXT | yes | Issue type (e.g. `Bug`, `Story`, `Task`) |
| `state` | TEXT | yes | |
| `fetched_at` | TEXT | no | ISO 8601 timestamp |

**Indexes**: UNIQUE(`provider`, `external_id`).

---

### `commit_work_items`

Many-to-many join table linking commits to work items.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `commit_id` | INTEGER | no | FK → `commits(id)` ON DELETE CASCADE |
| `work_item_id` | INTEGER | no | FK → `work_items(id)` ON DELETE CASCADE |

**PK**: (`commit_id`, `work_item_id`).

---

### `classification_overrides`

Manual Tier 0 overrides. Entries here take absolute priority over all rule-based and
LLM classifications. Managed via `tga override add|list|remove`.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `commit_sha` | TEXT | no | Commit SHA |
| `repo` | TEXT | yes | Repository name scope; NULL = applies to all repos |
| `work_type` | TEXT | no | Override work type |
| `change_type` | TEXT | no | Override change type |
| `notes` | TEXT | yes | Justification/notes |
| `created_at` | TEXT | no | ISO 8601 timestamp |

**Indexes**: UNIQUE(`commit_sha`, `repo`).

---

### `collection_runs`

Per-(repo, ISO year, ISO week) collection bookkeeping. Presence of a row signals that
this (repo, year, week) tuple has already been collected. The `--force` flag bypasses
this check and allows re-collection.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `repo_name` | TEXT | no | Repository name |
| `iso_year` | INTEGER | no | ISO year (e.g. `2025`) |
| `iso_week` | INTEGER | no | ISO week number 1–53 |
| `collected_at` | TEXT | no | ISO 8601 timestamp |
| `commit_count` | INTEGER | no | Number of commits collected; default 0 |
| `repo_count` | INTEGER | no | Size of `repositories[]` at write time; default 0 |

**Indexes**: UNIQUE(`repo_name`, `iso_year`, `iso_week`), INDEX(`repo_name`, `iso_year`, `iso_week`).

`repo_count` (added in migration `0009`) enables week-over-week baseline drift detection
when repository lists change.

---

### `repository_analysis_status`

Per-repository tracking of pipeline state. One row per repository name.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `repo_name` | TEXT PK | no | Repository name |
| `last_collected_at` | TEXT | yes | ISO 8601 timestamp |
| `last_classified_at` | TEXT | yes | ISO 8601 timestamp |
| `last_reported_at` | TEXT | yes | ISO 8601 timestamp |
| `total_commits` | INTEGER | no | Total commits in DB for this repo |
| `classified_commits` | INTEGER | no | Commits with a classification |
| `classification_coverage_pct` | REAL | yes | `classified / total * 100` |
| `config_hash` | TEXT | yes | BLAKE3 hash of config inputs; used to detect config drift |

---

### `azdo_iterations`

Azure DevOps iteration/sprint data.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `iteration_id` | TEXT | no | ADO iteration ID |
| `name` | TEXT | yes | Iteration display name |
| `project` | TEXT | no | ADO project name |
| `start_date` | TEXT | yes | ISO 8601 date |
| `finish_date` | TEXT | yes | ISO 8601 date |
| `fetched_at` | TEXT | no | ISO 8601 timestamp |

**Indexes**: UNIQUE(`iteration_id`, `project`).

---

### `pr_reviewers`

Per-PR reviewer records. Supports ADO reviewer votes; the `provider` column allows
future expansion to GitHub review requests.

| Column | Type | Nullable | Notes |
|---|---|---|---|
| `id` | INTEGER PK | no | Autoincrement |
| `pr_id` | INTEGER | no | FK → `pull_requests(id)` ON DELETE CASCADE |
| `provider` | TEXT | no | `azdo`; future: `github`; default `azdo` |
| `reviewer_id` | TEXT | no | Upstream identity ID |
| `display_name` | TEXT | yes | Human-readable name |
| `vote` | INTEGER | yes | ADO vote: 10=approved, 5=approved-with-suggestions, 0=no-vote, -5=waiting, -10=rejected |
| `is_required` | INTEGER | yes | Boolean — whether reviewer approval is required |
| `is_container` | INTEGER | yes | Boolean — whether entry represents a group/team |

**Indexes**: UNIQUE(`pr_id`, `provider`, `reviewer_id`), INDEX(`pr_id`).

---

## Migration History

Migrations are applied in order from `src/core/db/sql/`. Never modify an applied
migration — always add a new one.

| File | Description |
|---|---|
| `0001_initial_schema.sql` | Core tables: `authors`, `commits`, `classifications`, `files`, `pull_requests` |
| `0002_linear_issues.sql` | `linear_issues` table |
| `0003_commits_ticketed.sql` | `ticketed` and `ticket_id` columns on `commits` |
| `0004_collection_runs.sql` | `collection_runs` table for per-week bookkeeping |
| `0005_work_items.sql` | `work_items` and `commit_work_items` tables |
| `0006_classification_overrides.sql` | `classification_overrides` table (Tier 0) |
| `0007_pr_metrics_and_backfill.sql` | PR metrics columns; `is_revert` on `commits` |
| `0008_azdo_iterations.sql` | `azdo_iterations` table |
| `0009_collection_runs_repo_count.sql` | `repo_count` column on `collection_runs` |
| `0010_pull_requests_provider.sql` | `provider` column and updated UNIQUE index on `pull_requests` |
| `0011_pr_reviewers.sql` | `pr_reviewers` table |
| `0012_repository_analysis_status.sql` | `repository_analysis_status` table |
| `0013_commits_complexity.sql` | `complexity` column on `commits` (1–5 scale) |

Future migrations continue from `0014_*.sql`.
