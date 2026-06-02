# trusty-git-analytics (`tga`) — Components

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** existing `requirements/` docs + code/tickets reconciliation, updated through v2.5.0

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

Per-subsystem specs. Each states **responsibility**, **key types** (with `src/`
paths), **current state**, and **gaps**. System framing is in
[ARCHITECTURE.md](./ARCHITECTURE.md); field-level detail is in
[`../requirements/`](../requirements/).

---

## 1. Collector (`src/collect/`)

**Responsibility.** Stage 1: extract commits from local git repos and enrich
them with PR / ticket / identity data, persisting to SQLite.

**Key types & modules.**
- `collect/collector.rs` — top-level orchestrator; per-(repo, ISO-week)
  `collection_runs` bookkeeping; per-repo error isolation (#334).
- `collect/git/{extractor,diff,fetch,reachability}.rs` — `git2` revwalk, in-process
  diff stats, HTTPS fetch (vendored TLS, #337), tag/release-branch reachability
  (`reachability.rs`, #279/#290/#303). `diff.rs` also exposes `diff_for_commit`
  (unified diff text, 200 KiB cap) for the contributor-profile pipeline (#559).
- `collect/ai_attribution.rs` (`detect_ai_tool`) — `Co-Authored-By:` trailer
  detection for Claude/Copilot/Cursor, called at INSERT time by `extractor.rs`;
  sets `commits.is_ai_assisted` / `commits.ai_tool` (#445 batch A).
- `collect/pr_provider.rs` (`PrProvider` trait) + `collect/{github,bitbucket,azdo}/`
  — concurrent PR collection (GitHub, Bitbucket Cloud, Azure DevOps).
- `collect/pm_adapter.rs` (`PmAdapter`/`PmTicket`/`PmSource`) — normalized ticket
  enrichment across JIRA / GitHub / Linear / Azure DevOps.
- `collect/{jira,linear}/client.rs` — ticket fetch clients.
- `collect/identity/resolver.rs` — manual → email → login → Jaro-Winkler → new
  canonical UUID.
- `collect/ticket.rs` — `extract_ticket_id` / `is_ticketed` regex extraction
  (per-provider `ticket_regex` override, #75/#285/#316).
- `collect/weeks.rs` — ISO-week range resolution.

**Current state.** ✅ All four PR providers, all PM adapters, identity cascade,
reachability, per-repo error isolation, AI co-authorship attribution, and
`diff_for_commit` are implemented. Branch coverage bug (#331, -56% on multi-repo
orgs) and always-fetch-before-collect (#334) are fixed.

**Gaps.** GitLab PR collection 🔵 (parity gap, `collection.md` §PR). Bitbucket
`merged_at` is approximated by `updated_on` (one-directional bias, ADR-0003 §6)
🟡. SSH transport disabled by design ✅/non-goal.

---

## 2. Classifier — cascade + tiers (`src/classify/`)

**Responsibility.** Stage 2: assign each commit a two-level work category with
provenance, via a first-match-wins multi-tier cascade.

**Key types & modules.**
- `classify/pipeline.rs` (`ClassificationPipeline`, `ClassificationStats`) —
  read DB → classify → write back; coverage stats per tier/category/repo; WAL
  checkpoint at exit (#298).
- `classify/classifier.rs` (`ClassificationEngine`, `ClassificationEngineConfig`)
  — cascade orchestrator.
- `classify/tiers/` — `override_tier.rs`, `issue_type_tier.rs`,
  `jira_project_tier.rs`, `exact.rs` (Aho-Corasick), `regex_tier.rs`, `fuzzy.rs`,
  `llm.rs`, `bedrock.rs`, `weighted_sum.rs`. Shared `ClassificationResult`
  (`top_level`, `category`, `subcategory`, `confidence`, `method`, `ticket_id`,
  optional LLM `complexity`).
- `classify/rules/` (`Rule`, `RuleSet`, `loader.rs`) — built-in + user YAML rules
  with working priority (#259); inspectable via `tga rules` (#209/#269).

**Current state.** ✅ Full cascade with the seven tiers above. Rule priority,
coverage gating, and crash-safe checkpointing are implemented.

**Gaps.** 🟡 `external_source` method not appearing in `classifications` despite
cache hits on some 1.5.2 paths (#320 open; related #321/#316 fixed).

---

## 3. Taxonomy (`src/classify/taxonomy.rs`)

**Responsibility.** Provide a stable, closed set of **top-level** work categories
plus an extensible registry of **subcategories** that roll up to them.

**Key types.**
- `TopLevelCategory` — closed enum: `Feature`, `Bugfix`, `Ktlo`, `Integrations`,
  `PlatformWork`, `Content`, `Maintenance`, + `Unknown`. Users **cannot** add to
  this list.
- `TaxonomyRegistry` (`with_builtins()`, `resolve()`) — subcategory-name →
  top-level lookup, seeded with built-ins and merged with user-defined
  `SubcategoryDef` entries from rules/config.
- `SubcategoryDef` — a user-declarable subcategory with its top-level parent.

**Current state.** ✅ Two-level taxonomy implemented; `bug_fix`/`bugfix`
collision fixed (#271).

**Gaps.** **Documentation drift** 🟡: `requirements/classification.md` still
describes a flat 19-value `ChangeType` enum and its fallthrough set
(`maintenance`/`ktlo`/`other`/`unknown`). The 19 names now live as *subcategories*
under the seven top-levels. The requirements doc should be refreshed or demoted
to "predecessor reference."

---

## 4. External classification sources (`src/classify/sources/`)

**Responsibility.** Use external ticket/alert systems as high-confidence
classification signals *ahead of* commit-message tiers — because a bare
`PROJ-1234 update handler` carries no semantic content but the ticket behind it
has an explicit issue type.

**Key types & modules.**
- `sources/mod.rs` (`SourceConfig`, `ExternalSignal`) — YAML-deserializable
  per-source config.
- `sources/resolver.rs` (`ExternalSourceResolver`) — per-run in-memory cache +
  dispatcher; extracts ticket keys, fetches only misses, returns the
  highest-priority signal across sources.
- Per-source clients: `jira.rs`, `github_issues.rs`, `linear.rs` (#272),
  `shortcut.rs` (#273), `confluence.rs` (#274), `datadog.rs` (#275).

**Current state.** ✅ Six sources implemented; confidence varies by signal
quality (#270); JIRA 403 (#286) and greedy-regex (#285) bugs fixed.

**Gaps.** `requirements/collection.md` documents only JIRA/GitHub/Linear/ADO
adapters; Shortcut / Confluence / Datadog are code-only additions 🟡 (doc drift).

---

## 5. Effort scorer (`src/core/effort.rs`, `src/core/effort_percentile.rs`)

**Responsibility.** Assign each commit a deterministic empirical effort T-shirt
size (absolute text label) and a corpus-relative integer band (1–5), both
persisted for per-engineer effort histograms and downstream warehouse joins.

**Key types.**
- `FORMULA_VERSION` (`"v1"`), `compute_effort(...)` → `EffortRecord` (size, score,
  loc, files, test_loc, tests_factor). (`core/effort.rs`)
- Formula: `score = α·log₂(LoC+1) + β·log₂(files+1) + δ·tests_factor`, with
  α=1.0, β=1.5, γ=0.0 (deferred), δ=1.0; `tests_factor = 1 − 0.3·min(test_LoC/LoC, 1)`.
- Static T-shirt bands (calibrated against 99 trusty-tools commits, PR #308):
  XS ≤ 6, S (6,10], M (10,14], L (14,18], XL > 18.
- Persisted to `fact_commit_effort` (`PRIMARY KEY (sha, repository)`,
  `formula_version`, unix `computed_at`, `effort_tshirt` INTEGER).
- `effort_tshirt_from_size(label) → i64` — static fallback mapping (XS=1…XL=5)
  used when the corpus is too small for percentile computation.
- `EffortPercentileThresholds` { p20, p40, p60, p80, sample_count } —
  corpus-level breakpoints; `band_for_score(score) → i64` (1–5). (`core/effort_percentile.rs`)
- `compute_percentiles(scores) → Option<EffortPercentileThresholds>` — nearest-rank
  method; returns `None` for corpora < 5 rows.
- `rebin_all(conn) → (rows_updated, Option<thresholds>)` — batch-updates
  `effort_tshirt` in `fact_commit_effort`; persists thresholds in
  `effort_percentile_thresholds`; falls back to static mapping on tiny corpora.
- `tshirt_for_score_incremental(conn, score, size_label)` — bins a single new
  commit against stored thresholds or falls back to static mapping.

**Current state.** ✅ v1 formula implemented; corpus-percentile binning implemented
(`tga backfill effort-tshirt`); static `size` TEXT and percentile `effort_tshirt`
INTEGER coexist in the table (intentional divergence by design); output identical
between Rust path and `scripts/compute-effort.sh`.

**Gaps.** 🔵 v2 formula with cyclomatic-complexity γ term (re-run inserts new rows
alongside v1 for score-evolution tracking). Not in `requirements/` — code-only.
🔵 Per-repo or per-team percentile datasets (the table supports it via `dataset`
PK but only `"default"` is used currently).

---

## 6. Quality scorer (`src/core/quality.rs`) & revert detector (`src/core/revert.rs`)

**Responsibility.** Per-engineer-per-week quality score (1–5) and the single
shared revert-detection heuristic feeding both `commits.is_revert` and the
report-time revert rate.

**Key types.**
- `core/quality.rs` — `quality = 0.35·(1−revert_rate) + 0.40·(1−bugfix_rate) +
  0.25·ticket_linkage_rate` (negative signals inverted so higher = better);
  bucketed into integer 1–5 with even 0.20 bands (#377).
- `core/revert.rs` — compiled `^revert` / `^fix.*revert` / `Revert "…"` /
  `revert(scope):` patterns anchored to the subject's first line; the single
  source of truth both the backfill and aggregator paths now call (resolves the
  ~87% disagreement that motivated #210/#377).

**Current state.** ✅ Both implemented and unified. Quality is now also persisted:
`Aggregator::persist_weekly_quality` UPSERTs each per-(author, year, week, repo)
row into `fact_weekly_quality` after every report run (#445 batch B, migration
`0018`). The grain table includes raw input counts (`revert_count`, `bugfix_count`,
`ticketed_count`, `commit_count`) and `formula_version` for downstream auditability.
`tga backfill quality` retroactively populates `fact_weekly_quality` for historical
data.

**Gaps.** Not in `requirements/` — code-only additions (doc drift now partially
addressed: quality is documented in this spec). The predecessor `fact_quality`
name never existed; `fact_weekly_quality` is the authoritative table name.

---

## 7. DORA subsystem (`src/commands/{deployments,incidents,dora}.rs`, migration `0014`)

**Responsibility.** Ingest deployment and incident events and compute the four
DORA delivery metrics with performance-level banding.

**Key types & tables.**
- `fact_deployments` (`deploy_id` PK, repo, environment, triggered/completed,
  status, git_sha, git_tag, source) — #212.
- `fact_incidents` (`incident_id` PK, source, detected/resolved, denormalised
  `mttr_hours`, severity, triggering_deploy, repo) — #213.
- `deployment_failures` (derived failure↔recovery commit join) — #208.
- SQL views: `v_deployment_frequency`, `v_lead_time`, `v_mttr`,
  `v_change_failure_rate`.
- Commands: `deployments collect` (`DeploymentsCollectArgs`), `incidents collect`
  (`IncidentsCollectArgs`), `dora` (`DoraArgs`).

**Current state.** ✅ Ingest + views + `tga dora` compute (lead time, deploy
frequency, MTTR, CFR) with Elite/High/Medium/Low banding. Datadog incident
extraction feeds MTTR (#213).

**Gaps.** `requirements/reporting.md` describes DORA as report-time arithmetic
only; the entire `fact_*` ingestion subsystem and the three commands are
code-only 🟡 (doc drift — the most significant gap between docs and code).

---

## 8. Reporter — aggregator, formatters, drill-down (`src/report/`)

**Responsibility.** Stage 3: turn classified/scored DB rows into CSV / JSON /
Markdown artifacts and per-engineer drill-downs.

**Key types & modules.**
- `report/aggregator.rs` (`Aggregator::build`) — single scan of `commits`
  ⨝ `classifications`, in-memory grouping into `ReportData`.
- `report/models.rs` — `ReportData`, `AuthorSummary`, `RepositorySummary`,
  `WeeklyMetrics`, `WeeklyVelocity`, `WeeklyCategorization`, `DoraMetrics`,
  `QualitySummary`, `VelocitySummary`, `DeveloperActivitySummary`,
  `UntrackedCommit`, plus the scope/health integers `repository_coverage`,
  `unresolved_authors`, `unresolved_author_commits` (#67/#68).
- `report/formatters/{csv,json,markdown}.rs` + `report/templates/` (Tera).
- `report/drilldown.rs` (`AuthorDrilldownData`, `EffortHistogram`,
  `format_markdown`/`format_json`, `PrMetrics`) — powers `tga author` (#325).
  `PrMetrics` now derives `Serialize + Deserialize` so it embeds in
  `AuthorPeriodSummary`.
- `report/ticketed_stats.rs` (`compute_ticketed_stats`, `TicketedStats`).
- `report/pipeline.rs` (`ReportPipeline`, `ReportStats`) — orchestrator.
- `report/period_trends/` — contributor-profile data pipeline (#558/#560):
  - `model.rs` — `AuthorPeriodSummary` (Serialize + Deserialize struct with
    `period_label`, `since`/`until`, `commit_count`, `categories`,
    `effort_histogram`, `quality_score`, `ticketed_pct`, `pr_metrics`,
    `repositories`).
  - `query.rs` — `query_author_period_trends(db, email, window_weeks, since, until)`
    partitions the author's ISO-week history into N-week buckets and aggregates
    existing WeeklyActivity / effort / quality / PR data into
    `Vec<AuthorPeriodSummary>`; reads existing schema, no migration needed.
  - `tests.rs` — 9 unit tests (basic windowing, label format, ticketed_pct,
    empty/unknown author, category aggregation, date filter, week-range helper).
  - Exported from `tga::report` as `pub use period_trends::{query_author_period_trends, AuthorPeriodSummary}`.

**Current state.** ✅ All CSV/JSON/Markdown outputs, velocity, weighted activity
scoring, boilerplate filter, anonymization, `--author` filter (#324), `tga author`
drill-down (#325), `fact_weekly_quality` UPSERT (#445 batch B), and the
contributor-profile data pipeline (`diff_for_commit` + `query_author_period_trends`,
#559/#560) are implemented.

**Gaps.** Activity-scoring weights, boilerplate thresholds match
`requirements/reporting.md` ✅. Markdown narrative templates documented as
shipping in `tga-report/templates/` in the requirements doc; they actually live
in `src/report/templates/` (single-crate consolidation) 🟡 (minor doc drift).

---

## 9. Database & migrations (`src/core/db/`)

**Responsibility.** Own the single SQLite file, its pragmas, the typed query
wrappers, and the versioned migration runner.

**Key types & modules.**
- `core/db/mod.rs` (`Database`, `Database::open`/`open_in_memory`,
  `CheckpointMode`) — WAL + `synchronous=NORMAL` + `foreign_keys=ON`.
- `core/db/migrations.rs` — idempotent runner over `sql/0001_*.sql … 0019_*.sql`,
  tracked in `schema_migrations`.
- Per-table wrappers: `collection_runs.rs`, `work_items.rs`,
  `azdo_iterations.rs` (no raw SQL at call sites).

**Current state.** ✅ Migrations `0001`–`0019`; WAL checkpointing on exit (#298);
rusqlite version aligned to workspace (#257). New tables since v2.3.0:
- `fact_weekly_quality` (migration `0018`) — persisted per-author-per-week quality.
- `effort_percentile_thresholds` (migration `0019`) — corpus p20/p40/p60/p80
  breakpoints for `effort_tshirt` assignment.
- Push-down columns (migration `0017`) — `classifications.top_level_category`,
  `fact_commit_effort.effort_tshirt`, `commits.is_ai_assisted` / `ai_tool`.

**Gaps.** 🟡 `requirements/database-schema.md` is stale: it lists migrations only
through `0013`, uses predecessor table names (`cached_commits`,
`weekly_fetch_status`), and omits the `fact_*` family, DORA views, push-down
columns, `fact_weekly_quality`, and `effort_percentile_thresholds`. The on-disk
`src/core/db/sql/` set is authoritative.

---

## 10. CLI & commands (`src/main.rs`, `src/commands/`)

**Responsibility.** Parse the clap CLI, open the DB, and dispatch to one `run`
function per subcommand.

**Key types & modules.**
- `main.rs` — `Cli` parser, global flags, `Commands` enum, ISO-week selectors,
  `LogLevel`; `RUST_LOG` precedence; shared `cli-help` (#216).
- `commands/` — `analyze.rs`, `collect.rs`, `classify.rs`, `report.rs`,
  `author.rs`, `pr_metrics.rs`, `aliases/` (crud + suggest), `backfill.rs`,
  `override_cmd.rs`, `rules.rs`, `install.rs`, `deployments.rs`, `incidents.rs`,
  `dora.rs`, plus shared `date_range.rs`.
- `backfill.rs` `BackfillSubcommand` enum: `TicketIds`, `RevertFlags`, `Effort`,
  `Reachability`, `Complexity`, `AiDetection`, `AiDetectionCommits`, `Ticketed`,
  `TopLevel`, `EffortTshirt`, `Quality`. All share global `--dry-run`, `--repos`,
  `--weeks`, `--since`/`--until` filters. (`--branch` not applicable: post-collect
  commits carry no branch attribution.)

**Current state.** ✅ Fifteen subcommands; eleven `tga backfill` sub-subcommands
(five pre-existing + six from #445); uniform `--repo`/`--branch`/`--week` filters
(#332); per-subcommand `--help` audit (#333).

**Gaps.** 🟡 `requirements/cli-commands.md` documents `fetch` and `identities
list/merge` as standalone subcommands (folded into `collect` / `aliases`) and
omits `author`/`backfill`/`rules`/`deployments`/`incidents`/`dora`. ARCHITECTURE
§5 has the reconciled table.

---

## 11. Configuration & LLM config (`src/core/config/`)

**Responsibility.** Load and validate the YAML config; own the `llm:` provider
layer and DB-path precedence.

**Key types & modules.**
- `core/config/mod.rs` (`Config`, `Config::load`, `expand_path`) — serde YAML,
  `~` + `${VAR}` expansion, unknown-key tolerance.
- `core/config/validator.rs` (`ConfigValidator`) — cross-field checks (activity
  weights sum to 1.0, etc.).
- `core/config/azdo.rs` — Azure DevOps config section.
- `core/config/aliases.rs` — identity-alias config.
- Top-level `database:` (#406) and `llm:` (`source`/`api_key_env`/`region`/`model`,
  #407) sections; self-enabling LLM; hard-fail on missing key env var (#405).

**Current state.** ✅ All sections in `requirements/configuration.md` plus the
three #405/#406/#407 additions implemented; secret values never persisted.

**Gaps.** None material; `requirements/configuration.md` was already updated for
`database:` and `llm:` and is the authoritative field reference. ✅

---

## 13. Contributor-profile data pipeline (`src/collect/git/diff.rs`, `src/report/period_trends/`)

**Responsibility.** Provide the tga data-supply layer for the longitudinal
per-contributor profiling epic (#558) so `trusty-review` can build LLM-backed
review profiles without duplicating git/DB logic.

**Key types & modules.**
- `collect::git::diff::diff_for_commit(repo_path, sha) → Result<String>` —
  opens the repo at `repo_path` with libgit2, resolves `sha`, computes the
  unified diff against the first parent (or empty tree for root commits), and
  returns the diff text capped at `DIFF_BYTE_CAP` (200 KiB) with a
  `[... diff truncated …]` marker when cut. 4 unit tests.
- `report::period_trends::AuthorPeriodSummary` — the per-period roll-up struct
  (serde Serialize+Deserialize). Fields: `period_label`, `since`, `until`,
  `commit_count`, `categories` (HashMap), `effort_histogram` (HashMap),
  `quality_score`, `ticketed_pct`, `pr_metrics` (`PrMetrics`), `repositories`.
- `report::query_author_period_trends(db, email, window_weeks, since, until) → Result<Vec<AuthorPeriodSummary>>` — resolves the author's canonical logins,
  enumerates their ISO weeks in `[since, until]`, partitions into `window_weeks`-
  wide buckets, and builds one `AuthorPeriodSummary` per bucket by querying
  existing `commits`, `fact_commit_effort`, `fact_weekly_quality`, and
  `pull_requests` rows. No schema change required.

**Current state.** ✅ Both functions implemented and tested; `period_trends`
module split into `mod.rs`/`model.rs`/`query.rs`/`tests.rs` to stay within the
500-line file cap. Public APIs exported from `tga::report`.

**Gaps.** The LLM-backed profiling logic (batch_reviewer, synthesizer, profile
CLI) lives in `trusty-review`, not tga. tga's responsibility ends at data supply.
trusty-review issues #561–#568 are BLOCKED-ON-MVP.

---

## 12. Errors (`src/{core,collect,classify,report}/errors.rs`)

**Responsibility.** Structured library errors (`thiserror`) and an `anyhow`
binary boundary.

**Key types.** `TgaError`/`Result` (core), `CollectError`, `ClassifyError`,
`ReportError` — each with `#[from]` conversions for upstream `git2` / `rusqlite`
/ `reqwest` errors. `commands/` and `main.rs` use `anyhow::Result`.

**Current state.** ✅ Per-module enums; no `unwrap()` in library code (the
cascade fallback uses a documented `expect()` invariant that rule-based always
returns `Some`).

**Gaps.** None material.
