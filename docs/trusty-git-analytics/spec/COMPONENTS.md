# trusty-git-analytics (`tga`) — Components

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** existing `requirements/` docs + code/tickets reconciliation

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
  (`reachability.rs`, #279/#290/#303).
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
reachability, and per-repo error isolation are implemented. Branch coverage bug
(#331, -56% on multi-repo orgs) and always-fetch-before-collect (#334) are fixed.

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

## 5. Effort scorer (`src/core/effort.rs`)

**Responsibility.** Assign each commit a deterministic empirical effort T-shirt
size, persisted for per-engineer effort histograms.

**Key types.**
- `FORMULA_VERSION` (`"v1"`), `compute_effort(...)` → `EffortRecord` (size, score,
  loc, files, test_loc, tests_factor).
- Formula: `score = α·log₂(LoC+1) + β·log₂(files+1) + δ·tests_factor`, with
  α=1.0, β=1.5, γ=0.0 (deferred), δ=1.0; `tests_factor = 1 − 0.3·min(test_LoC/LoC, 1)`.
- T-shirt bands (calibrated against 99 trusty-tools commits, PR #308):
  XS ≤ 6, S (6,10], M (10,14], L (14,18], XL > 18.
- Persisted to `fact_commit_effort` (`PRIMARY KEY (sha, repository)`,
  `formula_version`, unix `computed_at`).

**Current state.** ✅ v1 implemented; output identical between Rust
(`tga backfill effort`) and `scripts/compute-effort.sh`.

**Gaps.** 🔵 v2 formula with cyclomatic-complexity γ term (re-run inserts new rows
alongside v1 for score-evolution tracking). Not in `requirements/` — code-only.

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

**Current state.** ✅ Both implemented and unified.

**Gaps.** Not in `requirements/` — code-only additions (doc drift). No persisted
`fact_quality` table; quality is computed at report time.

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
  `format_markdown`/`format_json`) — powers `tga author` (#325).
- `report/ticketed_stats.rs` (`compute_ticketed_stats`, `TicketedStats`).
- `report/pipeline.rs` (`ReportPipeline`, `ReportStats`) — orchestrator.

**Current state.** ✅ All CSV/JSON/Markdown outputs, velocity, weighted activity
scoring, boilerplate filter, anonymization, `--author` filter (#324), and the
`tga author` drill-down (#325) are implemented.

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
- `core/db/migrations.rs` — idempotent runner over `sql/0001_*.sql … 0016_*.sql`,
  tracked in `schema_migrations`.
- Per-table wrappers: `collection_runs.rs`, `work_items.rs`,
  `azdo_iterations.rs` (no raw SQL at call sites).

**Current state.** ✅ Migrations `0001`–`0016`; WAL checkpointing on exit (#298);
rusqlite version aligned to workspace (#257).

**Gaps.** 🟡 `requirements/database-schema.md` is stale: it lists migrations only
through `0013`, uses predecessor table names (`cached_commits`,
`weekly_fetch_status`), and omits the `fact_*` family and DORA views. The on-disk
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

**Current state.** ✅ Fifteen subcommands; uniform `--repo`/`--branch`/`--week`
filters (#332); per-subcommand `--help` audit (#333).

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
