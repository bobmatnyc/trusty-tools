# trusty-git-analytics (`tga`) — Architecture

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** existing `requirements/` docs + code/tickets reconciliation, updated through v2.5.0

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational

This document describes the system shape: the processing pipeline, the SQLite
schema, the LLM configuration layer, and the CLI surface. Field-level detail
lives in [`../requirements/`](../requirements/); per-subsystem detail lives in
[COMPONENTS.md](./COMPONENTS.md).

---

## 1. Crate shape

`tga` is a **single crate** (`crates/trusty-git-analytics/`, package name `tga`,
edition 2021, MSRV 1.88, license Elastic-2.0) consolidated from an earlier
5-crate workspace. The library (`src/lib.rs`) and the binary (`src/main.rs`,
`[[bin]] name = "tga"`) are both built from it. The `bedrock` Cargo feature gates
the AWS Bedrock LLM provider; everything else is in the default build.

```
src/
├── lib.rs                 # public library surface: core / collect / classify / report
├── main.rs                # clap CLI; wires commands into the library
├── core/                  # shared foundation (config, db, models, errors, effort, quality, revert)
├── collect/               # Stage 1: git walk + external PR/ticket fetch + identity
├── classify/              # Stage 2: multi-tier cascade + external sources + taxonomy
├── report/                # Stage 3: aggregation + CSV/JSON/Markdown formatters + drill-down
└── commands/              # binary-private clap subcommand handlers
```

Module dependency order: `core` ← {`collect`, `classify`, `report`} ← `commands`
← `main.rs`. `tga` depends on `trusty-common` only for the shared `cli-help`
feature (declarative `help.yaml` + Jaro-Winkler "did you mean?" suggester,
#216); it is otherwise independent of the rest of the workspace.

---

## 2. Processing pipeline

The pipeline is **`collect → classify → score → aggregate → report`**, all stages
reading from and writing to one SQLite file. `tga analyze` runs them in sequence;
each stage is also independently invocable. SQLite WAL mode lets a later stage
read while an earlier one is still writing.

```
┌──────────────┐   ┌───────────────┐   ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│  collect     │──▶│  classify     │──▶│  score       │──▶│  aggregate   │──▶│  report      │
│ (Stage 1)    │   │ (Stage 2)     │   │ (effort/     │   │ (DB → in-mem │   │ (CSV/JSON/MD │
│              │   │               │   │  quality)    │   │  ReportData) │   │  + drilldown)│
│ git2 revwalk │   │ override →    │   │ fact_commit_ │   │ single scan, │   │ Tera         │
│ + rayon      │   │ ext-source →  │   │ effort;      │   │ in-mem group │   │ templates,   │
│ PR/ticket    │   │ jira-key →    │   │ revert/      │   │ DORA views   │   │ csv::Writer, │
│ fetch        │   │ exact/regex/  │   │ bugfix/      │   │              │   │ serde_json   │
│ identity res │   │ fuzzy → LLM   │   │ ticket rates │   │              │   │              │
└──────┬───────┘   └──────┬────────┘   └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
       │                  │                   │                  │                  │
       ▼                  ▼                   ▼                  ▼                  ▼
  ┌──────────────────────────────────────────────────────────────────────────────────┐
  │   tga.db  (single SQLite file, WAL; synchronous=NORMAL; foreign_keys=ON)           │
  └──────────────────────────────────────────────────────────────────────────────────┘
```

> **Reconciliation note.** `requirements/overview.md` describes a 3-stage
> pipeline (collect → classify → report) over two DB files (`gitflow_cache.db` +
> `identities.db`). The code uses **one** SQLite file (`tga.db`) and inserts an
> explicit **score** step (effort + quality) and an **aggregate** step (the
> `report::Aggregator`) inside what the requirements lump into "report". This
> spec uses the 5-phase framing because it matches the modules.

### 2.1 Stage 1 — collect (`src/collect/`)

- **Git walk** (`collect/git/`): `git2` revwalk over each repo, in-process diff
  stats (`diff.rs`), unified diff text for contributor-profile callers
  (`diff.rs::diff_for_commit`, 200 KiB cap, #559), branch selection
  (`extractor.rs`), HTTPS fetch (`fetch.rs`, vendored-TLS libgit2 #337),
  tag/release-branch reachability (`reachability.rs`, #279). Parallel across
  repos/branches with rayon.
- **AI attribution** (`collect/ai_attribution.rs`): `detect_ai_tool(message)` scans
  `Co-Authored-By:` trailers at insert time; sets `commits.is_ai_assisted` /
  `commits.ai_tool` (#445 batch A).
- **External fetch**: GitHub (`collect/github/`), Bitbucket Cloud
  (`collect/bitbucket/`, ADR-0003), Azure DevOps (`collect/azdo/`, WIQL +
  `workitemsbatch`), JIRA (`collect/jira/`), Linear (`collect/linear/`) — all
  behind the `PrProvider` (`pr_provider.rs`) and `PmAdapter` (`pm_adapter.rs`)
  traits, dispatched concurrently from `tokio::JoinSet`.
- **Identity** (`collect/identity/`): manual → exact-email → GitHub-login →
  `strsim::jaro_winkler` fuzzy → new canonical UUID.
- **Bookkeeping**: `collector.rs` writes `collection_runs` per (repo, ISO-week);
  per-repo errors are isolated (#334).

### 2.2 Stage 2 — classify (`src/classify/`)

The cascade (`pipeline.rs` → `classifier.rs`) runs, first-match-wins:

1. **Manual override** (`tiers/override_tier.rs`) — `classification_overrides`.
2. **External source / issue type** (`sources/` + `tiers/issue_type_tier.rs`) —
   resolves the ticket behind the commit (JIRA / GitHub Issues / Linear /
   Shortcut / Confluence / Datadog) via `ExternalSourceResolver` (per-run cache)
   and maps issue type → category.
3. **JIRA project key** (`tiers/jira_project_tier.rs`) — `jira_project_mappings`.
4. **Exact** (`tiers/exact.rs`) — Aho-Corasick multi-keyword FSM.
5. **Regex** (`tiers/regex_tier.rs`) — pre-compiled patterns.
6. **Fuzzy** (`tiers/fuzzy.rs`) — structural heuristics (merge/revert/ticket).
7. **LLM** (`tiers/llm.rs`, `tiers/bedrock.rs`) — async, batched, circuit-broken,
   confidence-gated fallback. `tiers/weighted_sum.rs` blends multi-signal scores.

Tiers 4–6 run in parallel across commits via rayon; the LLM tier is async/serial.
Every verdict carries a `ClassificationResult { top_level, category, subcategory,
confidence, method, ticket_id, complexity }`. WAL is checkpointed at exit (#298).

### 2.3 Stage 3 — score, aggregate, report (`src/report/`, `src/core/`)

- **Score**: `core/effort.rs` (deterministic v1 formula → `fact_commit_effort`,
  via `tga backfill effort`); `core/effort_percentile.rs` (corpus-percentile
  binning → `effort_tshirt` integer 1–5 in `fact_commit_effort`, via `tga backfill
  effort-tshirt`, #445 batch C); `core/quality.rs` (per-engineer-per-week 1–5,
  now also persisted to `fact_weekly_quality` via `Aggregator::persist_weekly_quality`,
  #445 batch B); `core/revert.rs` (shared revert detector — single source of truth for
  `is_revert` and report-time revert rate).
- **Aggregate**: `report/aggregator.rs` does a **single** scan of `commits`
  left-joined to `classifications`, grouping in memory into `ReportData`
  (`report/models.rs`). DORA arithmetic reads the SQL views. After aggregation
  `Aggregator::persist_weekly_quality` UPSERTs per-(author, year, week, repo)
  quality rows into `fact_weekly_quality` (#445 batch B).
- **Report**: `report/formatters/{csv,json,markdown}.rs` + Tera templates
  (`report/templates/`); `report/drilldown.rs` powers `tga author`;
  `report/ticketed_stats.rs` computes ticket-linkage stats;
  `report/period_trends/` (`mod.rs`, `model.rs`, `query.rs`) provides
  `query_author_period_trends` + `AuthorPeriodSummary` for the contributor-profile
  pipeline (#558, closes #559/#560).

---

## 3. Database schema

Detailed table-by-table source: [`../requirements/database-schema.md`](../requirements/database-schema.md)
(note: that doc lists migrations `0001`–`0013`; the **on-disk** migration set
runs to `0019`). Every `Database::open()` applies:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;
```

Migrations live in `src/core/db/sql/0001_*.sql … 0019_*.sql`, tracked in
`schema_migrations` and applied idempotently by `src/core/db/migrations.rs`.

### 3.1 Table families

| Family | Tables | Source |
|---|---|---|
| **Commits & identity** | `authors`, `commits`, `files`, `classifications`, `classification_overrides` | `0001`, `0003`, `0006`, `0013` |
| **External work items** | `work_items`, `commit_work_items`, `linear_issues`, `azdo_iterations` | `0002`, `0005`, `0008` |
| **Pull requests** | `pull_requests` (+ `provider`, `repository`), `pr_reviewers` | `0007`, `0010`, `0011`, `0012` |
| **Run bookkeeping** | `collection_runs` (+ `repo_count`), `repository_analysis_status` | `0004`, `0009` |
| **DORA facts** (#207/#208/#212/#213) | `fact_deployments`, `fact_incidents`, `deployment_failures` + views `v_deployment_frequency`, `v_lead_time`, `v_mttr`, `v_change_failure_rate` | `0014` |
| **Reachability** (#279) | `fact_commit_reachability` (default-branch / tag / release-branch) | `0015` |
| **Effort** (PR #308) | `fact_commit_effort` (XS–XL size, score, LoC, files, test_loc, `formula_version`) | `0016` |
| **Push-down columns** (#445 batch A) | `classifications.top_level_category` TEXT; `fact_commit_effort.effort_tshirt` INTEGER (1–5); `commits.is_ai_assisted` INTEGER (0/1) + `commits.ai_tool` TEXT | `0017` |
| **Quality persistence** (#445 batch B) | `fact_weekly_quality` (grain: author × iso_year × iso_week × repository; `quality_score`, `quality_tshirt`, raw counts, `formula_version`, `computed_at`) | `0018` |
| **Effort percentile store** (#445 batch C) | `effort_percentile_thresholds` (p20/p40/p60/p80 breakpoints per dataset, `sample_count`, `computed_at`) | `0019` |

The `fact_*` convention: each row is an immutable observation keyed by a stable
upstream id (e.g. `deploy_id`, `(sha, repository)`), so re-ingest is idempotent
and the fact tables can be re-built independently of commit collection. Most
`fact_*` tables intentionally carry **no** FK to `commits` so a backfill can run
before or independently of the collection walk.

> **Reconciliation note.** The requirements DB schema uses the predecessor names
> `cached_commits` / `weekly_fetch_status`; the live schema uses `commits` /
> `collection_runs`. The nine `fact_*` tables, DORA views, push-down columns,
> `fact_weekly_quality`, and `effort_percentile_thresholds` do not appear in
> the requirements doc at all. Treat the on-disk SQL under `src/core/db/sql/` as
> authoritative.

---

## 4. LLM configuration layer

The LLM fallback tier reaches a provider through the top-level `llm:` config
section (`core/config/`, #407) — the single source of truth, superseding the
legacy `classification.llm_provider` / `classification.openrouter_api_key`
fields (which emit a deprecation `tracing::warn!`).

```yaml
llm:
  source: openrouter            # openrouter | bedrock | anthropic-api
  api_key_env: OPENROUTER_API_KEY   # NAME of the env var — never the secret itself
  region: us-east-1             # Bedrock only; falls back to AWS SDK resolution
  model: gpt-4o-mini            # provider-appropriate default if omitted
```

| Source | Auth | Default model | Availability |
|---|---|---|---|
| `openrouter` (default) | key from `api_key_env` (OpenAI-compatible schema) | `gpt-4o-mini` | default build |
| `bedrock` | AWS IAM credential chain (no stored secret) | `anthropic.claude-3-haiku-20240307-v1:0` | requires `--features bedrock` |
| `anthropic-api` | `x-api-key` from `api_key_env`, `anthropic-version: 2023-06-01` | `claude-3-5-haiku-latest` | default build |

Behavioral rules (#405/#407):

- **Self-enabling**: a present `llm:` section enables the LLM tier automatically
  (no `classification.use_llm: true` needed). Precedence: `llm:` → legacy
  `use_llm` → disabled.
- **Hard-fail on missing key**: when LLM is enabled but the named env var is
  unset/empty, `tga classify` exits non-zero with an actionable message naming
  the variable, **before** any DB write (no silent short-circuit — the bug #405
  was reported against).
- **Bedrock without the feature**: returns a configuration error explaining the
  build flag.
- **Security**: only the env-var *name* is ever stored; the secret is read at
  runtime.

All providers share one `SYSTEM_PROMPT` and the `LlmVerdict` JSON shape
(`category`, `subcategory`, `confidence`, `complexity`). The DB path resolves
`--database` > `database:` config (#406) > hardcoded `tga.db`.

---

## 5. CLI surface

Binary `tga` (clap v4 derive, `src/main.rs`). Global flags: `--config`/`-c`,
`--database`/`-d`, `-v`/`--log` (with `RUST_LOG` taking precedence). ISO-week
targeting via mutually-exclusive `--weeks` / `--week` / `--from`+`--to`.

| Subcommand | Stage / role | Source |
|---|---|---|
| `analyze` | Full pipeline (`--skip-collect`, `--skip-classify`, `--force`). | `commands/analyze.rs` |
| `collect` | Stage 1 — git walk + external fetch (`--repo`/`--branch`/`--week`, #332). | `commands/collect.rs` |
| `classify` | Stage 2 — cascade (`--reclassify`, `--validate-coverage`, `--show-jira-signals`). | `commands/classify.rs` |
| `report` | Stage 3 — reports (`--author <email>` #324, `--anonymize`). | `commands/report.rs` |
| `author` | Per-engineer drill-down (effort + PRs + commits, #325). | `commands/author.rs` |
| `pr-metrics` | Weekly PR-metrics aggregation. | `commands/pr_metrics.rs` |
| `aliases` | Identity CRUD + LLM-assisted `suggest` (#347/#348). | `commands/aliases/` |
| `backfill` | Retroactive maintenance ops: `ticket-id` / `revert-flags` / `effort` / `reachability` / `complexity` / `ticketed` / `ai-detection` / `ai-detection-commits` / `top-level` / `effort-tshirt` / `quality` (`--dry-run`, `--repos`, `--weeks`, `--since`, `--until`). | `commands/backfill.rs` |
| `override` | Manual Tier-0 classification overrides. | `commands/override_cmd.rs` |
| `rules` | Inspect/validate the active rule set (#209/#259). | `commands/rules.rs` |
| `install` | Interactive config wizard. | `commands/install.rs` |
| `deployments collect` | Ingest deploy events → `fact_deployments` (#212). | `commands/deployments.rs` |
| `incidents collect` | Ingest incidents → `fact_incidents` (#213). | `commands/incidents.rs` |
| `dora` | Compute & display the four DORA metrics. | `commands/dora.rs` |

> **Reconciliation note.** `requirements/cli-commands.md` documents `fetch` and
> `identities list/merge` as standalone subcommands and omits `author`,
> `backfill`, `rules`, `deployments`, `incidents`, and `dora`. The live CLI
> folds fetch into `collect` and identity management into `aliases`, and adds the
> six newer commands. The table above reflects the audited `main.rs`.

---

## 6. Key technology choices

Detailed rationale: [`../requirements/rust-architecture.md`](../requirements/rust-architecture.md)
and [`../decisions/`](../decisions/).

| Concern | Choice | Why (short) |
|---|---|---|
| Git | `git2` (vendored libgit2, https + vendored-openssl, no SSH) | mature diff/revwalk; self-contained TLS (#337) |
| Database | `rusqlite` (bundled + `chrono`) | sync API maps to per-thread rayon workers; workspace version pin (#257) |
| Async | `tokio` + `reqwest` (rustls-tls) | HTTP fetch for PR/ticket APIs |
| CPU parallelism | `rayon` | per-repo / per-branch / per-commit-batch parallelism |
| Pattern match | `aho-corasick` | one FSM pass for all classification keywords |
| Fuzzy match | `strsim` (Jaro-Winkler) | identity resolution |
| Config | `serde` + `serde_yaml` + `shellexpand` | typed YAML, `~` + `${VAR}` expansion |
| Dates | `chrono` | ISO-week support matching Python `isocalendar()` |
| Reports | `csv`, `serde_json`, `tera` | buffered/streamed output |
| Hashing | `blake3` | config-drift hash (`repository_analysis_status.config_hash`) |
| LLM (Bedrock) | `aws-config` + `aws-sdk-bedrockruntime` (feature `bedrock`) | IAM-auth provider |
| Errors | `thiserror` (library) / `anyhow` (binary) | structured per-module enums; no `unwrap()` in lib |

---

## 7. Cross-cutting conventions

- **No `unwrap()` in library code**; `thiserror` enums per module
  (`CollectError`, `ClassifyError`, `ReportError`, `TgaError`), `anyhow` in
  `commands/` and `main.rs`.
- **Logs to stderr** (tracing); stdout reserved for human-readable report text.
- **Forward-compatible config**: unknown YAML keys are ignored so newer configs
  load on older binaries.
- **Determinism**: effort scores carry a `formula_version`; identical output is
  guaranteed between the Rust path and `scripts/compute-effort.sh`.
- **Idempotent ingest**: `fact_*` tables keyed by stable upstream ids; migrations
  idempotent and never edited after release (additive `0014`–`0019` only).
- **Library-stable APIs for downstream consumers**: `diff_for_commit` and
  `query_author_period_trends` are stable public library functions callable from
  trusty-review without any tga CLI involvement.
