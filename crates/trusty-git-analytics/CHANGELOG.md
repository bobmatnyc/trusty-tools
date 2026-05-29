# Changelog

All notable changes to trusty-git-analytics will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.2.1] - 2026-05-29

### Fixed

- **`tga classify` no longer hangs ~120s at exit with external sources enabled
  (#397, bug 1)** — `tga` used the default multi-threaded Tokio runtime via
  `#[tokio::main]`. On return the runtime drop blocks until every background
  task finishes, including `reqwest`'s keep-alive connection-pool reaper, whose
  idle sockets against a live server (e.g. JIRA/Atlassian) linger up to the
  ~90s pool idle timeout. The process therefore completed all work, printed its
  summary, then hung before terminating. `main` now builds the runtime
  explicitly and calls `Runtime::shutdown_timeout(0)` after the async body
  completes, dropping idle connection tasks immediately so the process exits
  promptly. All real work is awaited before shutdown, so nothing is discarded.

- **`tga classify` no longer fails with "database is locked" immediately after
  `tga collect` (#397, bug 3)** — connections were opened without a
  `busy_timeout`, so a `classify` that opened while a just-finished `collect`
  was still flushing/checkpointing its WAL hit `SQLITE_BUSY` and failed
  instantly. Every connection now sets `PRAGMA busy_timeout = 5000`, so a
  transient checkpoint lock is waited out (up to 5s) instead of erroring.

- **`classifications.complexity` (1–5) is now populatable via a discoverable
  command (#397, bug 2)** — the complexity scoring logic shipped in 2.2.0
  (the LLM tier produces scores; the pipeline persists them) but was only
  reachable via the non-obvious `tga classify --backfill-complexity` flag. The
  normal `tga classify` run only consults the LLM for low-confidence commits,
  so rule-/JIRA-resolved commits keep `complexity = NULL`. A new
  `tga backfill complexity` subcommand exposes the existing
  `ClassificationPipeline::backfill_complexity` operation where operators look
  for it. It fills every `complexity IS NULL` (non-`exact_rule`) row via the
  LLM, supports `--dry-run` and `--use-llm`, and leaves
  category/confidence/method untouched. (No new scoring feature was built; this
  is a wiring/discoverability fix.)

## [2.2.0] - 2026-05-29

### Added

- **Developer Quality metric (#377)** — weekly reports now include a per-developer
  Quality score (1–5 scale) alongside existing velocity metrics. New columns added
  to weekly reporting output:

  - `revert_count` — number of revert commits authored in the reporting window
  - `bugfix_count` — number of commits classified as bugfix
  - `ticketed_count` — number of commits that reference a ticket ID
  - `quality_score` — composite numeric score (1–5) derived from the above signals
  - `quality_tshirt` — human-readable t-shirt size label (`A`–`E` mapped from score)
  - `abandoned_pr_count` — pull requests opened but never merged within the window

- **Unified revert detection** — revert commits are now identified consistently
  across all report types using a single shared regex-based detector. Previously
  revert heuristics were duplicated across classify and report stages.

- **Abandoned-PR tracking** — `tga collect` now records the opened-at timestamp
  for pull requests. The weekly reporter counts PRs that have been open for more
  than a configurable threshold (default: 14 days) without merging as
  "abandoned," surfacing stale work in team quality snapshots.

## [2.1.1] - 2026-05-28

### Fixed

- **HTTPS remote fetch support** (#337). Enable `git2` `https` and `vendored-openssl`
  features so `tga collect` can fetch from `https://` remotes without an external
  system `git` pre-fetch. The always-fetch behavior shipped in 2.0.0 now works for
  the common case of GitHub/GitLab https remotes:
  - macOS: uses the native Security framework (no openssl runtime dependency)
  - Linux: vendors openssl statically (portable binaries, no system libssl)
  - Windows: uses Schannel (no openssl runtime dependency)
- The pre-2.1.1 workaround (pre-fetch with system git, then `tga collect --no-fetch`)
  is no longer necessary for https remotes.

## [2.1.0] - 2026-05-28

### Added

- **`tga author <email>` per-engineer drill-down subcommand (#325)** — produces a
  focused report for a single canonical identity covering four sections:

  - **Commit Summary** — total commits, ticket coverage fraction, repositories
    touched, first/last commit dates, total insertions/deletions.
  - **Effort Histogram** — XS/S/M/L/XL bucket counts from `fact_commit_effort`
    (populated by `tga backfill effort`), with a "N / M commits scored" coverage
    header so readers know if the histogram is incomplete.
  - **Pull Request Metrics** — total PRs, merged PRs, avg/median/p95 cycle time
    (hours). Cycle times are filtered to the [0.5, 720] hour window matching
    the existing velocity computation. p95 is omitted when fewer than 20 merged
    PRs are present in scope.
  - **Category Breakdown** — per-category commit counts (feature, bugfix,
    maintenance, etc.) from the classifications table.

  Supports `--format markdown` (default, human-readable tables) and
  `--format json` (machine-readable, suitable for CI dashboards). Both `--since`
  and `--until` (ISO8601 `YYYY-MM-DD`) are included in the MVP to scope the
  report to a specific time window.

- **`tga aliases add-login <email> <provider> <login>` subcommand (#325)** —
  appends a provider login (e.g. GitHub username) to `authors.aliases` so that
  `tga author` can match pull-request authorship across providers without a
  schema migration. Provider is validated against the allow-list
  (`github`, `gitlab`, `ado`, `bitbucket`). Duplicate logins are silently
  ignored (idempotent). This is a one-time setup operation per developer per
  provider:
  ```bash
  tga aliases add-login alice@example.com github alice-gh
  tga aliases add-login bob@example.com github bobm
  ```

### Implementation notes

- PR canonicalization uses option (a) from the spec: provider logins are stored
  in `authors.aliases` as non-email entries (no `@`). The PR query extracts
  logins from the JSON array at query time. No schema migration is required.
- Median and p95 cycle time are computed in Rust by sorting the raw duration
  vector — SQLite has no native `MEDIAN` aggregate.
- The effort histogram join path: `fact_commit_effort.sha → commits.sha →
  commits.author_id → authors.canonical_email`.
- When `tga author` is called and no PRs are found, a note in the PR Metrics
  section instructs the user to run `tga aliases add-login` to map provider
  logins.

## [2.0.0] - 2026-05-28

### BREAKING CHANGES

- **`tga collect` now walks ALL local branches and remote tracking refs by
  default** (`refs/heads/*` + `refs/remotes/origin/*`), not just HEAD ancestry
  (#331). This fixes a data-integrity bug where commits on non-default branches
  (PR branches, feature branches, hotfixes) were silently excluded — the
  HEAD-only walk was losing ~56% of commits in multi-branch repos.

### Added

- **`--head-only` flag on `tga collect`** — restores the legacy HEAD-only walk
  (≤ 1.5.4 behaviour) for all repositories when passed on the command line.
  For a per-repo opt-out, set `head_only: true` in the `repositories[].head_only`
  field of your YAML config.

- **`--branch <NAME[,NAME…]>` flag on `tga collect` (#332)** — restrict the
  revwalk to one or more branch names (comma-separated). Seeds from both
  `refs/heads/<name>` and `refs/remotes/origin/<name>`, so local and
  remote-tracking copies are both covered. Mutually exclusive with `--head-only`.
  Per-repo branch overrides in YAML (`repositories[].branch`) are unchanged and
  still take precedence over the per-repo logic, but the new `--branch` flag
  takes precedence over everything for the current CLI invocation.
  Example: `tga collect --branch main,release/1.0 --repos my-service`

- **Uniform `--repos`, `--weeks`, `--since`, `--until`, `--force` filters on
  `tga classify` (#332)** — `tga classify` now accepts the same date/repo filter
  flags used by `tga collect`, enabling surgical re-classification of a specific
  slice without touching the full database:
  ```bash
  tga classify --force --repos my-service --weeks 4
  tga classify --force --since 2026-01-01 --until 2026-03-31
  ```
  Supplying `--since`/`--until`/`--weeks` without `--force` emits a `WARN`
  (the filter is a no-op for new commits but is a footgun without `--force`).

- **Uniform global filter flags on all `tga backfill` subcommands (#332)** —
  `--repos`, `--weeks`, `--since`, and `--until` are now declared as `global`
  flags on `BackfillArgs`, scoping every subcommand (ticket-ids, revert-flags,
  reachability, effort) uniformly. The old per-subcommand `--repo` flag on
  `reachability` is replaced by the global `--repos` (plural, comma-separated).
  ```bash
  tga backfill ticket-ids --repos api --since 2026-01-01
  tga backfill effort --repos core --weeks 8 --force
  tga backfill reachability --repos my-service
  ```

- **Per-repo fetch visibility with `--strict-fetch` and `--verbose-fetch` (#334)** —
  `tga collect` now tracks the outcome of the pre-walk `git fetch` for every
  repository and prints an end-of-run fetch summary to stderr:

  ```
  Fetch summary: 116 / 118 repos updated (2 failure(s), 0 skipped)
    - ml_pricing_engine: could not find remote 'origin'
    - datapipelines: authentication required
  ```

  Successful fetches are omitted from the summary unless `--verbose-fetch` is
  passed. `--strict-fetch` causes `tga collect` to exit non-zero after the
  summary if any repository had a fetch failure — useful for CI pipelines that
  treat stale-data as a hard error.

  When `--no-fetch` is active, a warning is printed to stderr at the start of
  collection:
  ```
  WARNING: --no-fetch active. Local clones may be stale. tga collect will walk
  only what's already in your local object store.
  ```

  The optional per-repo `repositories[].fetch_timeout_secs: <N>` config field
  stores a per-repo fetch timeout in seconds; enforcement via a watchdog thread
  is scheduled for a future release.

- **Comprehensive CLI documentation (#333)** — every subcommand now has:
  - `about` (one-line description shown in `tga --help`)
  - `long_about` (multi-paragraph context shown in `tga <subcommand> --help`)
  - `after_help` with 2-3 concrete examples and `TIPS` lines
  - Per-flag `help` strings on every newly-added flag
  Subcommands with new docs: `analyze`, `classify`, `report`, `backfill`,
  `pr-metrics`, `install`, `aliases`, `override`, `rules`, `deployments collect`,
  `incidents collect`, `dora`.

- **README "Common Workflows" section (#333)** — the README now opens with a
  "Common Workflows" section covering first-time setup, routine weekly runs,
  scoped re-runs, branch-restricted collection, rule tuning, backfill operations,
  and the DORA metrics pipeline. A "CLI Subcommand Reference" table lists every
  subcommand with its purpose and key flags.

### Migration

Existing `tga.db` files were collected with the buggy HEAD-only walker and are
missing commits from non-default branches. To recover the full commit history:

```bash
tga collect --force
```

The `--force` flag bypasses the `collection_runs` skip mechanism, allowing
already-"collected" weeks to be re-walked with the new all-branches coverage.
Expect significantly more commits per week after re-collecting (the bug was
losing ~56% of commits on multi-repo orgs).

### Internal

- New `head_only: bool` field on `GitCollector` and `CollectionPipeline` with
  `with_head_only(bool)` builder method on each.
- Revwalk seeding logic in `extractor.rs::collect_window` now has three arms:
  explicit `branch` override (unchanged), `head_only = true` (legacy escape
  hatch), and the new default (push all `refs/heads/*` + `refs/remotes/origin/*`
  except `refs/remotes/origin/HEAD` which is a symref).
- The all-branch walk falls back to `HEAD` for repos with a detached HEAD and
  no local branches (common in CI shallow clones).
- Regression tests added for all four scenarios: all-branch coverage,
  `--head-only` legacy behaviour, explicit `branch:` override, and
  detached-HEAD fallback (tests in `collect::git::extractor::tests`).

## [1.5.4] - 2026-05-28

### Added

- **`--author <email>` filter on `tga report` (#324)** — Scope any report run
  to a single canonical identity by passing `--author <canonical_email>`.  The
  filter matches case-insensitively against the `canonical_email` field in the
  `authors` table, so `ALICE@EXAMPLE.COM` and `alice@example.com` are
  equivalent.  All formatters (CSV, JSON, Markdown) receive the single-engineer
  `ReportData` unchanged — no formatter logic was modified.  When the supplied
  email does not match any canonical identity, `tga report` exits non-zero with
  a helpful message directing the user to `tga aliases list`.
  Omitting `--author` preserves the existing full-team aggregate behaviour.

## [1.5.3] - 2026-05-27

### Fixed

- **`JiraProjectTier` (Tier 1.6) and `IssueTypeTier` (Tier 1.5) misreport classification method (#319)** — Commits classified via `jira.jira_project_mappings` (Tier 1.6) were persisted with `method = 'regex_rule'` instead of `'external_source'`, causing JIRA project-key-driven verdicts to be attributed to the regex bucket in analytics dashboards. Similarly, PM issue-type-driven verdicts (Tier 1.5, e.g. JIRA `"Bug"` → `bugfix`, Linear `"Story"` → `feature`) were misreported as `'exact_rule'`. Both tiers now correctly emit `ClassificationMethod::ExternalSource` since classification is driven by external ticket-system metadata, not commit-message text patterns. The regression became observable after the 1.5.2 fix to inline `commits.ticket_id` population (#316) — once users could see which commits had JIRA ticket IDs, the misattributed method in `classifications.method` was detectable. No schema migration required; re-running `tga classify` on affected date ranges will repopulate the correct method values.

## [1.5.2] - 2026-05-27

### Fixed

- **`commits.ticket_id` NULL for extractable JIRA IDs during `tga collect` (#316)** — 32.3% of uncategorized commits (2,006 of 6,212) had clearly extractable JIRA ticket IDs in their commit messages (e.g. `BB-2746`, `SRE-3104`, `DRE-405`) but NULL `ticket_id` because extraction only happened in `tga backfill ticket-ids`, not at INSERT time during `tga collect`. `extract_ticket_id` is now called inline during collection so `commits.ticket_id` is populated immediately without a separate backfill pass. The `tga backfill ticket-ids` subcommand is retained for patching legacy databases.

### Changed

- **`extract_ticket_id` moved to `tga::collect::ticket`** — The function was previously a private implementation detail of `src/commands/backfill.rs`. It is now a public API in `tga::collect::ticket` (alongside `is_ticketed`) so it can be reused by the collection pipeline. The `backfill` module now imports from `ticket` rather than maintaining its own duplicate.

## [1.0.12] - 2026-05-19

### Added

- **Native complexity scoring 1–5 with DB migration 0013 and `--backfill-complexity` flag (#97)** — Commits are now scored for complexity on a 1–5 scale using a native classifier. Schema migration 0013 adds the `complexity` column to the `commits` table, and the new `--backfill-complexity` flag allows retroactive scoring of existing commits in the database.

## [1.0.11] - 2026-05-19

### Fixed

- **LLM fallback bypasses cascade short-circuit (#99)** — The four-tier classification cascade was exiting early on exact-rule and regex matches but still invoking the LLM fallback path in some edge cases. The fallback is now gated correctly so it only runs when all preceding tiers produce no result.
- **Gate ADO `merge_commit_sha` by merge strategy (#96)** — The Azure DevOps PR fetcher was writing `lastMergeCommit.commitId` as `merge_commit_sha` regardless of merge strategy. The value is now only persisted when the PR was completed via merge (not squash or rebase), matching the semantics expected by commit-to-PR linkage.
- **Gate GitHub `merge_commit_sha` on `merged_at` (#101)** — The GitHub PR fetcher could write a non-null `merge_commit_sha` for PRs that were closed without merging. The field is now only set when `merged_at` is non-null, preventing phantom commit-to-PR associations.

## [1.0.10] - 2026-05-18

### Fixed
- **Honour `pm.azure_devops.ticket_regex` in work-item extraction (#90)** — The `ticket_regex` field in the ADO PM adapter config block was parsed but not applied during work-item ID extraction from commit messages. The adapter now compiles and uses the user-supplied pattern, consistent with the JIRA, GitHub, and Linear adapters.
- **Persist ADO PR merge commit SHA in `commit_shas` (#92)** — Azure DevOps PRs were fetched and stored without recording the merge commit SHA in the `commit_shas` join table, breaking commit-to-PR linkage. The fetcher now writes the `lastMergeCommit.commitId` value into `commit_shas` when present.
- **ADO PR fetcher supports multiple projects (#91)** — The Azure DevOps PR fetcher previously collected PRs from only the first project in the config. It now iterates over all configured projects, merging results with partial-success semantics (a failure on one project is logged at `WARN` level but does not abort collection for others).

### Added
- **`--force-refresh-prs` flag to backfill ADO `commit_shas` (#95)** — A new `--force-refresh-prs` flag on `tga collect` forces re-fetching of all ADO pull requests regardless of their cached state, enabling operators to backfill `commit_shas` rows that were missing due to the bug fixed in #92.

## [1.0.9] - 2026-05-15

### Fixed
- **Critical data loss bug in `pull_requests` table** (#88): `UNIQUE(provider, pr_number)` constraint
  caused ~62% silent row loss when collecting PRs from multiple repositories (GitHub resets
  `pr_number` per repo). Fixed by adding `repository TEXT NOT NULL DEFAULT 'unknown'` column and
  replacing the unique index with `UNIQUE(provider, repository, pr_number)`.
  Existing databases are migrated automatically (rows default to `repository = 'unknown'`;
  correct values written on next `tga collect` run).

## [1.0.8] - 2026-05-15

### Fixed

- **ADO connectionData probe now uses api-version=7.1-preview.1 (#85)** — The connectivity probe sent during ADO initialization previously used a deprecated API version, causing intermittent auth failures on newer Azure DevOps organizations. The probe now sends `api-version=7.1-preview.1` for consistent compatibility.
- **ADO workitemsbatch now sends errorPolicy=omit (#86)** — The batch work-items endpoint previously returned a full-batch 404 when any single work item ID was invalid or inaccessible. Requests now include `errorPolicy=omit` so valid items in the batch are returned even when some IDs cannot be resolved.

### Added

- **GitHub PR fetcher supports org-wide / multi-repo configs (#87)** — The GitHub PR collection stage now accepts an `org` field and a `repositories[]` list in addition to the existing single-repo `repo` field. Repos are fetched concurrently with partial-success semantics: a failure on one repository is logged at `WARN` level but does not abort collection for the remaining repositories.

## [1.0.7] - 2026-05-15

### Added

- **Bitbucket Cloud PR provider (#72)** — pull-request collection now supports
  Bitbucket Cloud alongside GitHub and Azure DevOps. All providers share a
  `PrProvider` trait and run concurrently via `tokio::task::JoinSet`.
  Configured via a new `bitbucket:` block (workspace, repo_slug, fetch_prs,
  and either a Bearer token or an Atlassian API token via Basic auth).
  Bitbucket PRs are persisted with `provider = 'bitbucket'` so they
  deduplicate correctly against the `(provider, pr_number)` unique index
  from migration `0010_pull_requests_provider.sql`. Bitbucket Server /
  Data Center remains unsupported.

## [1.0.6] - 2026-05-14

### Fixed
- **`llm_fallback_threshold` config field now wired (#78)** — The `classification.llm_fallback_threshold` config value was parsed but never forwarded to `ClassificationEngine`. Commits above the threshold now correctly skip the LLM fallback tier.
- **ADO 404 error message clarified (#80)** — Azure DevOps 404 responses previously surfaced a generic HTTP error. The message now identifies the missing resource and suggests checking the project/organization slug in config.
- **`ticket_regex` config wired for JIRA, GitHub, and Linear adapters (#75)** — The `ticket_regex` field defined in each PM adapter's config block was ignored; all three adapters now compile and apply the user-supplied regex pattern when detecting ticket references in commit messages.
- **ADO workitemsbatch omit policy: dropped IDs now logged (#81)** — When the ADO batch API silently omits work item IDs (e.g. items in areas the token cannot read), `tga` now logs each dropped ID at `WARN` level so operators can identify access-control gaps without digging through raw HTTP responses.

### Performance
- **Parallel LLM fallback with `buffer_unordered` (#83)** — The per-commit LLM classification loop was fully serial; each request waited for the previous one to complete. Requests are now dispatched concurrently using `futures::stream::buffer_unordered`, capped at the configurable `llm_fallback_concurrency` limit (default 4), cutting wall-clock classification time roughly proportional to API latency.

### Added
- **ADO pull request fetcher with reviewer tracking (#84)** — `AzureDevOpsClient` now implements `fetch_pull_requests`, collecting PR metadata (title, state, author, dates, target branch) and the full reviewer list (identity, vote, required flag) into the `pull_requests` and `pr_reviewers` tables (migration `0011_pr_reviewers.sql`). Enabled per repository via `pm.azure_devops.fetch_prs: true`.

### Tests
- **Ticket-regex detection coverage (#76)** — Added unit tests for `ticket_regex` detection across the JIRA, GitHub, and Linear adapters, covering match, no-match, and malformed-pattern cases.

## [1.0.5] - 2026-05-12

### Fixed
- **GitHub PR fetch resilience (#73)** — `fetch_pull_requests` now routes through the same `retry_request` helper used by every other paginated GitHub endpoint. A single 429 or 5xx response no longer fails the entire PR collection run; transient errors are retried with exponential backoff.
- **Pull-request deduplication (#71)** — `INSERT OR REPLACE INTO pull_requests` was a silent no-op: the existing `idx_pull_requests_pr_number` index was non-UNIQUE, so SQLite's conflict resolution never fired and PRs accumulated on every re-run. Migration `0010_pull_requests_provider.sql` adds a `provider` column (default `'github'`) and a UNIQUE index on `(provider, pr_number)`. `store_pull_requests` now writes the provider explicitly so deduplication works correctly per provider.

## [1.0.4] - 2026-05-12

### Added
- **Self-analysis example config** (`configs/self-analysis.yaml`) — a minimal config that runs `tga` against its own repository, useful as a quickstart template and for dogfooding releases.

### Changed
- Documentation cleanup pass: refreshed `CLAUDE.md` implementation state date, verified ADR README and README.md version references are current.

## [1.0.3] - 2026-05-12

### Fixed
- **Bedrock LLM provider no longer warns about missing API key** — Bedrock uses the AWS credential chain; API key validation now correctly skips for the `bedrock` provider.

### Added
- **ADR documentation** — `docs/adr/README.md` explains the format and process; `docs/adr/TEMPLATE.md` provides a copy-paste starter. Format is referenced from `docs/requirements/rust-architecture.md` and `tga --help`.

## [1.0.2] - 2026-05-12

### Fixed
- **Timezone-aware ISO week assignment** (#70): commits timestamped late on the last day of an ISO week in a negative UTC offset timezone (e.g. -0700) were incorrectly placed in the following week due to UTC conversion. Collection window now uses the commit's local calendar date.
- **`until_date` inclusivity**: date range upper bound is now treated as end-of-day inclusive.

## [1.0.1] — 2026-05-12

Final pre-release polish: data-quality guards for multi-repo analysis,
identity-resolution honesty, and a clean CI gate.

### Added

- **Multi-repo coverage warning (#67)** — `ReportData` now carries a
  `repository_coverage` field counting the distinct `commits.repository`
  values observed in a run. The `analyze` command warns when this is
  smaller than the configured `repositories[]` roster, so a misconfigured
  scope no longer silently undercounts the portfolio.
- **Phantom-developer guard (#68)** — `unresolved_authors` and
  `unresolved_author_commits` are now tracked separately from the headline
  developer counts. Commits whose author identity does not resolve to a
  configured canonical team member are surfaced rather than inflating
  active-developer tallies.
- **WoW baseline drift detection (#69)** — `collection_runs` gains a
  `repo_count` column (migration `0009_collection_runs_repo_count.sql`)
  recording the size of `repositories[]` at the moment each row was
  written. Week-over-week comparisons can now detect when the prior
  baseline week was collected against a different repository roster and
  refuse to draw spurious deltas.
- **Restored `--log` flag (#66)** — `tga --log <path>` re-introduces the
  pre-consolidation log redirection contract; all `tracing` output is
  written to the supplied file in addition to stderr.

### Fixed

- **Clippy `unnecessary_sort_by`** — 8 closures of the shape
  `sort_by(|a, b| a.field.cmp(&b.field))` rewritten as `sort_by_key`,
  using `std::cmp::Reverse` for descending sorts. `cargo clippy
  --all-targets -- -D warnings` is now clean.
- **Rustdoc broken intra-doc links** — Fixed eight stale links left over
  from the workspace consolidation. All references to `crate::models`,
  `crate::aggregator`, and `crate::formatters` now point at their
  consolidated paths under `crate::report::*`; bare `TopLevelCategory` and
  `CollectError` references are fully qualified; private items
  (`Self::members`, `Database::apply_pragmas`) use code formatting instead
  of doc-links. `cargo doc --no-deps` is now warning-free.
- **`duetto_contractors_config_resolves` identity resolver test** —
  Verified passing against the bundled `configs/duetto-contractors.yaml`
  fixture; "Andre Ramos" and the other listed contractors resolve
  correctly via the configured alias map.

### Documentation

- `docs/requirements/database-schema.md` — Added the `collection_runs`
  table definition and the new `repo_count` column; documented the Rust
  port's re-indexed migrations (`0001`–`0009`).
- `docs/requirements/reporting.md` — Documented the new
  `repository_coverage`, `unresolved_authors`, and
  `unresolved_author_commits` fields on `ReportData`.

## [1.0.0] — 2026-05-12

First stable release. `trusty-git-analytics` is now feature-complete as a Rust port of
[`gitflow-analytics`](https://github.com/bobmatnyc/gitflow-analytics): every analytical
output the Python tool produces is reproduced by `tga` from the same YAML config, against
the same SQLite schema, with materially better performance and a single static binary.

### Highlights

- **Single `tga` crate** — consolidated from the original 5-crate workspace into one
  library + binary for faster builds, simpler dependency graph, and a cleaner public API.
- **8 CLI subcommands**: `analyze`, `collect`, `classify`, `report`, `init`,
  `validate-config`, `migrate`, `override` (plus auxiliary `aliases`, `backfill`,
  `pr-metrics`, `identities`, `install`, `fetch`).
- **7-tier classification cascade**: manual override → issue type → JIRA project mapping
  → exact (Aho-Corasick) → regex → fuzzy heuristics → LLM fallback, with both **AWS
  Bedrock** and **OpenRouter** as supported LLM providers (Bedrock behind the `bedrock`
  cargo feature).
- **9 CSV reports** plus DORA, velocity, and quality summaries:
  `weekly_metrics`, `developer_activity_summary`, `summary`, `untracked_commits`,
  `weekly_categorization`, `weekly_velocity`, `weekly_dora_metrics`, `authors`,
  `weekly_activity` — with `velocity_summary.json`, `quality_summary.json`, and
  `dora_summary.json` alongside.
- **Full data collection layer**: libgit2 commit extraction, identity resolution
  (exact + Jaro-Winkler fuzzy + email local-part normalized), GitHub REST client
  (PRs, reviews, commits, issues), JIRA Cloud client (JQL batch search, story points),
  Linear GraphQL client, Azure DevOps REST client (work items, iterations, users,
  comments, custom fields, commit links).
- **SQLite with WAL** on every open: `journal_mode=WAL`, `synchronous=NORMAL`,
  `foreign_keys=ON`, `cache_size=-65536`, `temp_store=MEMORY`, `mmap_size=256 MiB`.
- **Schema migration runner** applying versioned SQL migrations v1–v18 (ported from
  Python predecessor) plus Rust-era additions, recorded transactionally in
  `schema_migrations`.
- **247 tests passing** — unit, integration, and doctests — with zero clippy warnings
  and `cargo fmt --check` clean.
- **Cross-platform release binaries** published from CI for five targets:
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
  `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

### Compatibility

- Config files written for `gitflow-analytics` load without modification — unknown keys
  are silently ignored.
- The on-disk SQLite schema is the same shape; existing `gitflow_cache.db` files from
  `gfa` can be opened by `tga` and auto-migrated forward.
- Classification rule files (YAML/JSON) are portable in both directions.

## [0.3.0] — 2026-05-12

### Added
- **Classification**: Tier 0 manual override lookup (`classification_overrides` table), Tier 1.5 issue-type classifier (12-entry map, 0.90 confidence), Tier 3 JIRA project key mapping (0.95 confidence), classification coverage metrics persisted to DB, `--validate-coverage` flag
- **LLM**: OpenRouter provider with required headers (`HTTP-Referer`, `X-Title`); AWS Bedrock provider feature-gated (`--features bedrock`, model `anthropic.claude-3-haiku-20240307-v1:0`)
- **Reports**: 9 CSV reports (weekly_metrics, developer_activity_summary, summary, untracked_commits, weekly_categorization, weekly_velocity, weekly_dora_metrics), DORA 4-band classifier, velocity/PR cycle time, composite activity scoring, quality/revert detection, boilerplate filter; `velocity_summary.json`, `quality_summary.json`, `dora_summary.json`
- **GitHub client**: `fetch_pr_reviews`, `fetch_pr_commits`, `list_issues`; exponential backoff on 5xx/429
- **JIRA client**: `search_issues` (batch JQL, 50/page), `get_story_point_field`; `JiraIssue.story_points`
- **Azure DevOps Phase 4**: `get_iterations`, `get_users`, `feed_azdo_users`→IdentityResolver; `azdo_iterations` table (migration v8)
- **Azure DevOps Phase 5**: `get_work_item_comments`, `get_work_item_extended` (custom fields, iteration/area path), `get_work_item_commit_links`
- **CLI**: `tga override add|list|remove`, `tga pr-metrics`, `tga install` wizard, `tga aliases list|merge`, `tga backfill ai-detection|revert-flags|ticket-ids`
- **git remote fetch** before revwalk with non-interactive SSH auth; `--no-fetch` to skip
- **`--from`/`--to`** date flags on `collect` and `analyze`; mutual exclusivity with `--weeks` enforced
- **`--dry-run`** on `collect` and `analyze` — routes writes to in-memory DB
- **`--validate-only`/`--no-validate`** flags; `ConfigValidator` with 9 error variants
- **SQLite tuning**: `cache_size=-65536`, `temp_store=MEMORY`, `mmap_size=268435456`, `synchronous=NORMAL`, `foreign_keys=ON`; documented in `docs/adr/0001-sqlite-tuning.md`
- **Criterion benchmarks**: 5 groups in `benches/tga_bench.rs` (classify throughput, aho-corasick, CSV gen, identity resolution, ISO weeks)
- **Cross-compilation** release workflow: 4 targets (x86_64/aarch64 Linux musl, macOS Intel/Apple Silicon)
- `docs/migration-from-python.md`, `docs/adr/0002-performance-hotspots.md`

### Fixed
- `since_date` config / `--weeks` flag now correctly limits git revwalk (was collecting full history)
- `weekly_fetch_status` incremental skip now works — bounded path entered for `(since, None)` range
- Warning + spinner emitted when full-history traversal is about to occur

## [0.2.0] — 2026-05-11

### Added
- Unified `PmAdapter` trait abstracting JIRA, GitHub, Linear, and Azure DevOps behind a common `fetch_ticket` / `detect_ticket_refs` / `health_check` interface (`src/collect/pm_adapter.rs`)
- Azure DevOps Phases 3–6: work item types, WIQL queries, batch fetch (200/chunk), AB# reference detection and enrichment; `AzureDevOpsAdapter::fetch_ticket` now fully implemented
- `--weeks N` flag on `collect` and `analyze` subcommands — limits collection to the last N weeks (overrides config `start_date`)
- `GitHubAdapter::fetch_ticket` — real implementation fetching individual GitHub Issues via REST API (was stub)
- `--dry-run` flag on `collect` and `analyze` — runs the full pipeline against an in-memory DB, making no on-disk writes
- SQLite persistence for work items: `work_items` table (migration v5), `commit_work_items` join table, and `upsert_work_item` / `link_commit_work_item` / `get_work_items_for_commit` / `list_work_items` DB operations

### Changed
- `AzureDevOpsAdapter::fetch_ticket` no longer stubs — returns populated `PmTicket` from batch work item fetch

## [Unreleased]

## [2026-05-11]

### Added

#### `src/collect/pm_adapter.rs` — unified PM adapter layer
- `PmAdapter` async trait (`fetch_ticket`, `fetch_tickets`, `detect_ticket_refs`, `health_check`) with `Send + Sync` bounds for use behind `Box<dyn PmAdapter>`
- `PmTicket` normalized ticket payload preserving the raw upstream JSON in `PmTicket::raw` for forward compatibility
- `PmSource` enum (`Jira`, `GitHub`, `Linear`, `AzureDevOps`) with stable `as_str()` labels
- `PmError` thiserror enum covering `Http`, `Auth`, `NotFound`, `RateLimited`, `Serialization`, `Config`, `Other` variants
- Concrete adapter wrappers: `JiraAdapter`, `GitHubAdapter`, `LinearAdapter`, `AzureDevOpsAdapter` — each delegates to the existing backend client
- `build_adapters(config)` factory: instantiates every adapter whose config section is present; skips missing or invalid sections with `tracing::warn!` rather than failing the whole pipeline
- Shared detection regexes: JIRA/Linear `KEY-N`, GitHub `#N` (non-hex), ADO `AB#N`

#### `src/collect/azdo/client.rs` — Azure DevOps Phases 3–6
- `get_work_item_types(project)` — fetch all work item types defined in a project (Phase 3)
- `get_fields(project)` — fetch all field definitions for a project (Phase 3)
- `run_wiql(project, query)` — execute an arbitrary WIQL query, returning `WiqlResult` with work item IDs and refs (Phase 4)
- `get_recent_work_item_ids(project, since)` — convenience WIQL wrapper for recently-updated items (Phase 4)
- `get_work_items(ids)` — batch-fetch work items by ID; chunks requests at 200 IDs per call per ADO API limits (Phase 5)
- `extract_work_item_refs(text)` — standalone function extracting bare `AB#N` integers from commit messages (Phase 6)
- `fetch_referenced_work_items(client, text)` — convenience wrapper: detects refs in text, fetches the work items, returns `Vec<AzdoWorkItem>` (Phase 6)
- `AzureDevOpsAdapter::fetch_ticket` is now fully implemented (strips `AB#` prefix, calls `get_work_items`, maps to `PmTicket`)

#### `--weeks N` flag (`src/main.rs`, `src/commands/collect.rs`, `src/commands/analyze.rs`)
- `--weeks <N>` added to both `tga collect` and `tga analyze` subcommands
- Computes `now − N weeks` as an RFC3339 lower bound and applies it to every configured repository's `since_date`, overriding any `start_date` in config
- `--since` (explicit ISO date) takes precedence over `--weeks` when both are supplied on `collect`

### Added

#### tga-core
- `Config` struct with full YAML deserialization via `serde_yaml`; compatible with the Python `gitflow-analytics` config schema
- `Config::load()` with tilde-expansion on all path fields
- `Config::validate()` enforcing at minimum one configured repository
- `Config::resolved_aliases()` unifying `developer_aliases` (Python-compat flat map) and `team.members` (structured roster)
- `RepositoryConfig`, `TeamConfig`, `TeamMember`, `OutputConfig`, `ClassificationConfig`, `GithubConfig`, `JiraConfig`, `AnalysisConfig`, `CacheConfig` structs
- `Database` wrapper with mandatory WAL journal mode, `synchronous=NORMAL`, and `foreign_keys=ON` applied on every open
- `Database::open()` and `Database::open_in_memory()` with automatic migration execution
- `Database::journal_mode()` and `Database::schema_version()` introspection helpers
- Versioned SQL migration runner (`db::migrations`): transactional application, idempotent, records `(version, name, applied_at)` in `schema_migrations`
- Migration v1 (`0001_initial_schema.sql`): creates `authors`, `classifications`, `commits`, `files`, `pull_requests`, and `schema_migrations` tables with appropriate indexes and foreign keys
- Domain models: `Commit`, `Author`, `Classification`, `FileChange`, `PullRequest`
- Enums: `ClassificationMethod` (`exact_rule`, `regex_rule`, `fuzzy_match`, `llm_fallback`, `manual`), `ChangeType` (`added`, `modified`, `deleted`, `renamed`), `PrState` (`open`, `closed`, `merged`)
- `TgaError` enum with `thiserror` covering I/O, SQLite, YAML, validation, and migration errors
- `tga_core::Result<T>` type alias
- 100% `#[warn(missing_docs)]` coverage; `cargo doc` passes with `RUSTDOCFLAGS="-D warnings"`

#### tga-collect
- `CollectionPipeline` orchestrator: sequential per-repo git extraction, author backfill, optional GitHub PR fetch
- `CollectionStats` reporting commits collected, authors resolved, PRs fetched, and per-repo non-fatal warnings
- `GitCollector`: opens a local repository via libgit2, validates existence and git-ness on construction, walks the default branch, extracts commit SHA/author/timestamp/message/diff-stats
- `IdentityResolver` with three-tier resolution: (1) exact alias match on email, (2) exact alias match on name, (3) Jaro-Winkler fuzzy match (default threshold 0.85), (4) raw passthrough
- `IdentityResolver::from_config()` preferring `developer_aliases` over `team.members`
- `IdentityResolver::upsert_author()` writing canonical rows to the `authors` table with `ON CONFLICT` upsert
- `GitHubClient`: REST client for fetching pull requests via `reqwest` + `tokio`; stores PR rows via `store_pull_requests()`
- JIRA client stub (`JiraClient`) for future issue fetch integration
- `CollectError` enum with `thiserror`

#### tga-classify
- `ClassificationEngine` combining all four tiers behind a single `classify()` async entry point and a `classify_batch()` Rayon-parallel sync entry point
- `ClassificationEngineConfig` with `use_llm`, `llm_model`, and `confidence_threshold` fields
- `ExactMatcher`: builds a single Aho-Corasick automaton from all rule keyword lists; O(n) scan per message
- `RegexMatcher`: compiles one `Regex` per pattern string; first match wins; extracts JIRA ticket IDs via `extract_ticket_id()`
- `FuzzyClassifier`: heuristic detection of merge commits (via `is_merge` flag or "Merge pull request" prefix) and revert commits (via "Revert" prefix)
- `LlmClassifier`: optional async OpenAI-compatible fallback; reads `OPENAI_API_KEY` from environment; silently no-ops if key is absent
- `ClassificationResult` struct carrying `category`, `subcategory`, `confidence`, `method`, `ticket_id`
- Built-in default ruleset (`default_rules()`): 15 rules covering conventional commit prefixes (`feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `ci`, `perf`, `style`, `build`, `revert`), breaking-change marker, JIRA-style ticket pattern, and keyword fallbacks (`bug`, `security`)
- `load_rules()`: YAML or JSON rules file loader (format detected by extension)
- `Rule` and `RuleSet` types with `id`, `category`, `subcategory`, `keywords`, `patterns`, `priority`, `confidence`
- `ClassificationPipeline` orchestrator: reads unclassified commits from DB, runs cascade, writes `classifications` rows, links `commits.classification_id`
- `ClassificationStats` with `total_commits`, `classified`, `by_method`, `by_category` breakdowns

#### tga-report
- `Aggregator`: reads `commits` + `classifications` + `authors` from DB and produces an in-memory `ReportData`
- `ReportData` with `generated_at`, `period_start`, `period_end`, `total_commits`, `total_authors`, `category_breakdown`, `authors`, `repositories`, `weekly_activity`
- `AuthorSummary` per-author rollup: name, email, commit count, insertions, deletions, files changed, category map, first/last commit timestamps
- `RepositorySummary` per-repo rollup: name, commit/author counts, insertions, deletions, top categories
- `WeeklyActivity` per-week/author/repo bucket: ISO week label, counts, insertions, deletions, category map
- CSV formatter: `write_author_csv()` → `authors.csv`, `write_weekly_csv()` → `weekly_activity.csv`
- JSON formatter: `write_json()` → `report.json` (serializes `ReportData` directly)
- Markdown formatter: `write_markdown()` → `report.md` via embedded Tera template
- `ReportPipeline` orchestrator: resolves output directory, dispatches to all configured formatters, returns `ReportStats` with files written
- Default output directory `./reports` when `output.directory` is unset
- All three formats emitted when `output.formats` is empty

#### tga-cli
- `tga` binary with four subcommands wired to the pipeline crates
- `tga analyze`: runs collect → classify → report in sequence; `--skip-collect` and `--skip-classify` flags for partial re-runs; `--output` override
- `tga collect`: `--repos` filter, `--since` / `--until` date overrides
- `tga classify`: `--rules` file override, `--use-llm` flag
- `tga report`: `--output` directory override, `--formats` comma-separated list
- Global `--config` (default `config.yaml`), `--database` (default `tga.db`), and `-v`/`-vv`/`-vvv` verbosity flags
- `tracing-subscriber` initialization from verbosity count: `WARN` / `INFO` / `DEBUG` / `TRACE`
- Graceful config-not-found fallback to `Config::default()`
- `anyhow::Result` error propagation to `main`

#### CI / CD
- GitHub Actions CI workflow (`ci.yml`): runs on push and PR to `main`; matrix over `stable` and `beta` toolchains; jobs: format check, Clippy with `-D warnings`, tests (skipping the integration test that requires a local git repo configured via `INTEGRATION_REPO_PATH`), rustdoc build with `RUSTDOCFLAGS="-D warnings"`, release binary build
- Concurrent-run cancellation via `concurrency.cancel-in-progress`
- Rust artifact caching via `Swatinem/rust-cache@v2`
- GitHub Actions publish workflow (`publish-tga-core.yml`): triggered by `tga-core-v*` tags or `workflow_dispatch`; dry-run gate, Clippy gate, then `cargo publish`; supports `dry_run` input to skip actual upload
- `CARGO_REGISTRY_TOKEN` secret required for actual publish

#### Integration
- `configs/example-config.yaml`: example config for analyzing multiple repositories with developer aliases, CSV + Markdown output
