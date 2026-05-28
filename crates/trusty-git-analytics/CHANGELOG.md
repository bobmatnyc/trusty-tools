# Changelog

All notable changes to trusty-git-analytics will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
