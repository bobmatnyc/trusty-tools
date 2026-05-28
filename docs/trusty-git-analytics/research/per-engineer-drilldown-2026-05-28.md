# Per-Engineer Drill-Down Spec (`tga author <email>`) — #325

**Status**: Draft for user review before implementation begins.
**Target release**: tga 1.6.0 (minor bump; adds net-new subcommand).
**Branch**: `spec-325-tga-drilldown`

---

## 1. Investigation Summary

### Where things live today

**`pull_requests` table and its `author` column**

The `pull_requests` table was created in migration v7
(`src/core/db/sql/0007_pr_metrics_and_backfill.sql`) and extended through v12.
The column definition from migration v1 is:

```sql
CREATE TABLE IF NOT EXISTS pull_requests (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    pr_number    INTEGER NOT NULL,
    title        TEXT NOT NULL,
    author       TEXT NOT NULL,   -- raw provider login
    state        TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    merged_at    TEXT,
    commit_shas  TEXT NOT NULL DEFAULT '[]'
);
```

The `author` column is populated at ingest time in
`src/collect/github/client.rs` (`GitHubClient::store_pull_requests`):

```rust
pr.author,   // = p.user.map(|u| u.login).unwrap_or_default()
```

`p.user.login` is the raw GitHub username (e.g. `"bobm"`, `"alice-dev"`). It is
never canonicalized through the `authors` table. The ADO PR fetcher
(`src/collect/azdo/pr_fetcher.rs`) follows the same pattern: it stores the ADO
`createdBy.uniqueName` string (typically a UPN or display name) directly in
`pull_requests.author`. There is no join between `pull_requests.author` and
`authors.canonical_name` / `authors.canonical_email` anywhere in the codebase.

**`fact_commit_effort` schema**

Added in migration v16 (`src/core/db/sql/0016_fact_commit_effort.sql`):

```sql
CREATE TABLE IF NOT EXISTS fact_commit_effort (
    sha             TEXT NOT NULL,
    repository      TEXT NOT NULL,
    size            TEXT NOT NULL,   -- 'XS' | 'S' | 'M' | 'L' | 'XL'
    score           REAL NOT NULL,
    loc             INTEGER NOT NULL,
    files           INTEGER NOT NULL,
    test_loc        INTEGER NOT NULL,
    tests_factor    REAL NOT NULL,
    formula_version TEXT NOT NULL DEFAULT 'v1',
    computed_at     INTEGER NOT NULL,
    PRIMARY KEY (sha, repository)
);
```

There is **no `author_id` column** in `fact_commit_effort`. The join path to
a canonical identity is:

```
fact_commit_effort.sha
    → commits.sha      (via commits.sha UNIQUE)
    → commits.author_id
    → authors.id
    → authors.canonical_email
```

This join is currently unused in any query; `tga author` will be the first
consumer.

**`authors.aliases` and the existing aggregator**

`authors.aliases` is a JSON-encoded `TEXT` column (`DEFAULT '[]'`), containing
an array of alias strings. In practice the array holds **email addresses and
display names** that map to the same person (populated by `tga aliases merge`).
There is no provider-login field. The aggregator in
`src/report/aggregator.rs` (`Aggregator::build_filtered`) resolves canonical
identity by joining `commits c LEFT JOIN authors a ON a.id = c.author_id` and
using `COALESCE(a.canonical_email, c.author_email)`. The `pull_requests` table
is only used for team-level velocity/DORA metrics and is never joined to
`authors`.

---

## 2. PR-Author Canonicalization — Recommended Option

Three options were evaluated:

**(a) Extend `authors.aliases` JSON array to include provider logins**
**(b) New join table `author_provider_logins(author_id, provider, login)`**
**(c) Fuzzy match `pull_requests.author` against `authors.canonical_name` at query time**

### Recommendation: Option (a)

Extend `authors.aliases` to include provider logins alongside email aliases.

**Rationale**: The `aliases` column already stores a mixed-type list (emails +
display names). Adding GitHub logins to that list requires zero schema migration
and zero new table maintenance. The resolver at query time already must parse
the JSON array; filtering to entries that look like logins (no `@` sign and not
an email) versus emails (`@` present) is trivial. Adding a login looks like:

```json
["alice@example.com", "alice-old@example.com", "alice-dev"]
```

Option (b) would be cleaner at scale, but for a single-team analytics tool the
added migration cost and join complexity are not justified. Option (c) is
rejected: display names and logins diverge silently (e.g. the GitHub login
`"bobm"` vs the canonical name `"Bob Matsuoka"`).

### Populating the mapping: `tga aliases add-login`

A new `tga aliases add-login <email> <provider> <login>` subcommand
appends the login to `authors.aliases` for the named canonical identity. This
is a one-time setup operation per developer per provider:

```
tga aliases add-login alice@example.com github alice-dev
tga aliases add-login bob@example.com github bobm
```

No new migration is needed. The existing UNIQUE constraint on
`authors.canonical_email` is untouched.

### JOIN shape at drill-down query time

```sql
-- Resolve the set of logins for the given canonical email.
-- Step 1: fetch the aliases JSON for the author.
SELECT id, aliases
FROM authors
WHERE LOWER(canonical_email) = LOWER(:email);

-- Step 2 (in Rust): parse the JSON, filter to non-email entries
-- (entries lacking '@' are treated as provider logins).
-- Let :logins be the resulting list, e.g. ['alice-dev', 'alice'].

-- Step 3: count + aggregate PRs.
SELECT
    COUNT(*)                                         AS total_prs,
    COUNT(CASE WHEN state = 'merged' THEN 1 END)     AS merged_prs,
    AVG(
        CASE
            WHEN state = 'merged' AND merged_at IS NOT NULL
            THEN (julianday(merged_at) - julianday(created_at)) * 24.0
        END
    )                                                AS avg_cycle_time_hours,
    -- median requires a subquery in SQLite; see section 5 below
    COUNT(DISTINCT repository)                       AS repo_count
FROM pull_requests
WHERE author IN (:logins)           -- placeholder list from Step 2
  AND (:since IS NULL OR created_at >= :since)
  AND (:until IS NULL OR created_at <= :until);
```

Because SQLite lacks array parameters, the Rust side must build the query
dynamically with one `?N` placeholder per login entry (bounded list; typical
engineer has 1–3 logins).

---

## 3. Effort Histogram Query

The join path through `commits.sha` is the only route from effort data to
author identity:

```sql
-- Per-engineer effort histogram (all sizes).
SELECT
    fce.size,
    COUNT(*) AS commit_count
FROM fact_commit_effort fce
JOIN commits c ON c.sha = fce.sha
                 -- AND c.repository = fce.repository  (add if needed)
JOIN authors a  ON a.id = c.author_id
WHERE LOWER(a.canonical_email) = LOWER(:email)
  AND (:since IS NULL OR c.timestamp >= :since)
  AND (:until IS NULL OR c.timestamp <= :until)
GROUP BY fce.size
ORDER BY
    CASE fce.size
        WHEN 'XS' THEN 1
        WHEN 'S'  THEN 2
        WHEN 'M'  THEN 3
        WHEN 'L'  THEN 4
        WHEN 'XL' THEN 5
        ELSE 6
    END;
```

**Output shape**: five buckets (XS / S / M / L / XL), each showing a count and
the percentage of total effort-scored commits. The report renders this as a
simple histogram table.

**Edge case — commits with no effort row**: a commit is effort-scored only if
`tga backfill effort` has been run. Commits without a row in `fact_commit_effort`
are silently excluded from the histogram. The report should show the count of
scored commits vs. total commits so the reader knows if the histogram is
incomplete:

```
Effort histogram (73 / 104 commits scored):
| Size | Count | % scored |
```

**Unscored bucket**: do not add a synthetic "unscored" row to the histogram;
instead surface the coverage fraction in the section header. This avoids
confusion about what "unscored" means.

---

## 4. Ticket Coverage Metric

**Definition**:

```
ticket_coverage = (commits WHERE ticketed = 1) / (total commits for this author)
```

The `ticketed` column on `commits` is populated by `tga backfill ticket-ids`.

**SQL**:

```sql
SELECT
    COUNT(*)                                     AS total_commits,
    COUNT(CASE WHEN ticketed = 1 THEN 1 END)     AS ticketed_commits,
    CAST(
        COUNT(CASE WHEN ticketed = 1 THEN 1 END) AS REAL
    ) / MAX(COUNT(*), 1)                         AS ticket_coverage_ratio
FROM commits c
JOIN authors a ON a.id = c.author_id
WHERE LOWER(a.canonical_email) = LOWER(:email)
  AND (:since IS NULL OR c.timestamp >= :since)
  AND (:until IS NULL OR c.timestamp <= :until);
```

**User-visible format**: `42 / 104 (40%)` — fraction followed by percentage.
Show both because the absolute count matters (a 100% rate on 2 commits is not
meaningful).

**Edge case — zero commits**: display `"no commits in scope"` and omit the
ratio. An empty report section is less confusing than `0 / 0 (NaN%)`.

---

## 5. PR Metrics

### Definitions

- **`total_prs`**: all rows in `pull_requests` where `author` is in the
  engineer's login set (see section 2), regardless of state.
- **`merged_prs`**: rows where `state = 'merged'`.
- **`avg_cycle_time_hours`**: mean of `(merged_at - created_at)` in hours, for
  merged PRs with both timestamps present. Filter to the range `[0.5, 720]`
  hours (matching the existing `compute_velocity_inputs` logic in the aggregator
  to exclude same-minute merges and stale PRs older than 30 days).
- **`median_cycle_time_hours`**: the p50 value of the same distribution.
  SQLite does not have a native `MEDIAN` aggregate; compute it in Rust by
  sorting the vector and taking `vec[len/2]`.
- **p95 cycle time**: included if the sample is large enough (≥ 20 merged PRs);
  omitted otherwise. In Rust: `vec[(len * 95) / 100]`.

**Edge case — no PRs, commits present**: emit a note in the PR Metrics section:
`"No pull requests found. Ensure provider logins are mapped via tga aliases
add-login."` This distinguishes "the engineer has no PRs" from "the login
mapping is missing."

**Edge case — no commits, PRs present**: this indicates the engineer uses PRs
but commits are not in scope (possibly a different collection window). Report
PR metrics normally; ticket coverage and effort histogram will show `"no
commits in scope"`.

---

## 6. Subcommand Shape

### CLI

```
tga author <email> [--format markdown|json] [--since <DATE>] [--until <DATE>]
```

- `<email>` — positional, required. Resolved case-insensitively against
  `authors.canonical_email`. If not found, exits non-zero with:
  `"no canonical identity with canonical_email '<email>' found. Run tga aliases list."`.
- `--format markdown|json` — default `markdown`. Markdown for human reading;
  JSON for programmatic consumers (CI dashboards, etc.).
- `--since <DATE>` / `--until <DATE>` — optional ISO8601 `YYYY-MM-DD` dates.
  When absent the report covers all data in the database. **MVP scope**: include
  both flags in the initial implementation (they reuse the date-range helpers
  already in `src/commands/date_range.rs`). Skipping them would make the report
  nearly useless for engineers with multi-year histories.

### Markdown output structure

```markdown
# Engineer Report: Alice Smith <alice@example.com>
Generated: 2026-05-28 | Period: 2025-01-01 – 2026-05-28

## Summary
| Metric             | Value                     |
|--------------------|---------------------------|
| Total commits      | 104                       |
| Repositories       | acme/api, acme/frontend   |
| First commit       | 2025-01-07                |
| Last commit        | 2026-05-22                |
| Ticket coverage    | 42 / 104 (40%)            |

## Effort Histogram (73 / 104 commits scored)
| Size | Count | % scored |
|------|-------|----------|
| XS   |    18 |     25%  |
| S    |    30 |     41%  |
| M    |    16 |     22%  |
| L    |     7 |     10%  |
| XL   |     2 |      3%  |

## Pull Request Metrics
| Metric                   | Value     |
|--------------------------|-----------|
| Total PRs                | 31        |
| Merged PRs               | 28        |
| Avg cycle time           | 14.3 h    |
| Median cycle time        | 9.1 h     |
| p95 cycle time           | 48.2 h    |

## Category Breakdown
| Category    | Commits | % total |
|-------------|---------|---------|
| feature     |      48 |    46%  |
| bugfix      |      21 |    20%  |
| maintenance |      18 |    17%  |
| refactor    |      12 |    12%  |
| docs        |       5 |     5%  |
```

The Category Breakdown section reuses the per-author category histogram already
computed in `Aggregator::build_filtered` (the `AuthorAcc.categories` map). It
is included in the drill-down output because it is cheap and contextually
useful.

### JSON output structure

```json
{
  "generated_at": "2026-05-28T10:00:00Z",
  "email": "alice@example.com",
  "name": "Alice Smith",
  "period": { "since": "2025-01-01", "until": "2026-05-28" },
  "commits": {
    "total": 104,
    "ticketed": 42,
    "ticket_coverage": 0.404,
    "repositories": ["acme/api", "acme/frontend"],
    "first_commit": "2025-01-07T09:12:00Z",
    "last_commit": "2026-05-22T16:44:00Z",
    "insertions": 12840,
    "deletions": 4321
  },
  "effort": {
    "scored_commits": 73,
    "total_commits": 104,
    "histogram": {
      "XS": 18,
      "S": 30,
      "M": 16,
      "L": 7,
      "XL": 2
    }
  },
  "pull_requests": {
    "total": 31,
    "merged": 28,
    "avg_cycle_time_hours": 14.3,
    "median_cycle_time_hours": 9.1,
    "p95_cycle_time_hours": 48.2
  },
  "categories": {
    "feature": 48,
    "bugfix": 21,
    "maintenance": 18,
    "refactor": 12,
    "docs": 5
  }
}
```

All numeric cycle-time fields are `f64` (hours, 2 decimal places in rendering).
`ticket_coverage` is `f64` in `[0.0, 1.0]`. If there are no merged PRs the
`avg_cycle_time_hours`, `median_cycle_time_hours`, and `p95_cycle_time_hours`
fields are `null`.

---

## 7. Implementation Phases

All phases land in a single `tga 1.6.0` release on branch
`feat-325-tga-author`. The sequence below is the recommended commit order.

### Phase 1 — Schema + aliases extension (S)
**1 commit.**
- Add `tga aliases add-login <email> <provider> <login>` subcommand to
  `src/commands/aliases.rs`. Parses the existing `aliases` JSON array, appends
  the login (deduplicating), writes back. No migration needed.
- Unit tests: login round-trips through JSON; duplicate login ignored.

### Phase 2 — Effort rollup query (S)
**1 commit.**
- Add `query_effort_histogram(db, email, since, until)` free function in a new
  `src/report/drilldown.rs` module. Returns `HashMap<String, u32>` (size →
  count) plus the total scored-commit count and total commits for the author.
- Unit tests: seed `commits + fact_commit_effort`, assert histogram counts.

### Phase 3 — PR metrics query (S)
**1 commit.**
- Add `query_pr_metrics(db, logins, since, until)` in `src/report/drilldown.rs`.
  Returns total, merged, avg/median/p95 cycle-time hours (or `None` when no
  merged PRs). Computes median/p95 in Rust after fetching the raw durations.
- Unit tests: seed `pull_requests` rows, assert counts and cycle-time stats.

### Phase 4 — `AuthorDrilldown` report model + formatters (M)
**1–2 commits.**
- Add `AuthorDrilldownData` struct in `src/report/models.rs` (or
  `src/report/drilldown.rs`).
- Add `format_markdown(data: &AuthorDrilldownData) -> String` and
  `format_json(data: &AuthorDrilldownData) -> String` in
  `src/report/formatters/`.
- Unit tests: format against a hand-crafted `AuthorDrilldownData`, assert
  table headers and key values are present.

### Phase 5 — `tga author` subcommand (S)
**1 commit.**
- Add `src/commands/author.rs` with `AuthorArgs` (clap struct: `email`,
  `--format`, `--since`, `--until`) and `run()`.
- Wire into `src/main.rs` `Commands` enum and `main()` dispatch.
- Integration test in `src/commands/author.rs` tests module: seed a small DB,
  call `run()`, assert non-empty output.

### Phase 6 — Documentation + CHANGELOG (XS)
**1 commit.**
- Add entry to `CHANGELOG.md`.
- Update `crates/trusty-git-analytics/README.md` with the new subcommand.
- Bump `Cargo.toml` version from `1.5.x` to `1.6.0`.

---

## 8. Open Questions for User Review

1. **PR-author canonicalization option**: This spec recommends option (a) —
   extending `authors.aliases` to hold provider logins, populated via a new
   `tga aliases add-login` subcommand. Do you prefer option (b) (new join table
   `author_provider_logins`) for cleaner schema separation, or option (a) for
   zero-migration simplicity?

2. **`--since` / `--until` in MVP vs follow-up**: The spec includes both date
   flags in the initial implementation (Phase 5). They reuse existing
   `date_range.rs` helpers and cost little. Should they be deferred to a
   follow-up to keep the scope tighter, or ship in 1.6.0?

3. **Category breakdown in drill-down**: The spec includes a Category Breakdown
   section (feature / bugfix / maintenance / etc.) because the data is already
   collected by `Aggregator::build_filtered`. Do you want it in the
   `tga author` output, or should the drill-down focus only on the four new
   metrics (effort histogram, ticket coverage, PR metrics, commit summary)?

4. **`p95` cycle time threshold**: The spec emits `p95_cycle_time_hours` only
   when the engineer has ≥ 20 merged PRs, to avoid a misleading statistic on
   small samples. Should the threshold be different (e.g., ≥ 5, or always
   emit with a caveat), or is ≥ 20 the right gate?
