# trusty-git-analytics (`tga`) — Product Requirements Document

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** existing `requirements/` docs + code/tickets reconciliation

**Status legend:** ✅ Implemented · 🟡 Partial · 🔵 Designed-not-built · ⚪ Aspirational
Each requirement is framed **Vision / Current / Gap**.

---

## 1. Vision & Mission

### North-star vision

> **tga turns raw git history into trustworthy engineering analytics** — who did
> what kind of work, how much effort it took, how well it shipped, and how the
> team delivers against DORA — from a single local binary over an embedded SQLite
> database, with **no daemon, no cloud, and no Python or Node runtime in the
> hot path.**

Where a manager today stitches together GitHub insights, a JIRA velocity board,
a hand-rolled spreadsheet of "lines changed," and nothing at all for "what *kind*
of work was this," tga collapses all of that into one reproducible pipeline. The
defining properties are: **work classification that does not depend on commit
messages alone** (it reaches through to the JIRA/Linear/GitHub-Issues ticket
behind the commit), **empirical effort and quality scores** that survive
re-runs deterministically, and a **single-file SQLite output** any downstream
warehouse (e.g. a CTO analytics DuckDB) can read directly.

### Mission

Deliver a CLI any engineering leader or IC can point at a set of repositories
and get back, in seconds, per-author / per-week / per-category / DORA / velocity
reports — reproducibly, offline, and without standing up any service.

### Lineage & divergence

tga began as a Rust port of the Python `gitflow-analytics` tool with the goal of
config/schema/CLI parity. It has since **diverged upward**: a two-level work
taxonomy, multi-source classification (six external systems), per-commit effort
scoring, per-engineer quality scoring, a full DORA fact-table subsystem, tag /
release-branch reachability, and a per-engineer drill-down command are all tga
capabilities the predecessor never had. Parity with the Python tool is therefore
a **historical motivation, not a current constraint** — where the code exceeds
the predecessor, the code wins.

---

## 2. Goals & Non-Goals

### Goals

| # | Goal | Status |
|---|---|---|
| G1 | **Three-stage pipeline** — `collect → classify → report`, runnable end-to-end (`tga analyze`) or stage-by-stage. | ✅ |
| G2 | **Parallel git extraction** — libgit2 (`git2`) revwalk with rayon across repos/branches; smart branch selection. | ✅ |
| G3 | **Multi-provider PR collection** — GitHub, Bitbucket Cloud, and Azure DevOps PRs + reviewer data behind one `PrProvider`/`PmAdapter` abstraction. | ✅ |
| G4 | **Multi-source classification** — JIRA, GitHub Issues, Linear, Shortcut, Confluence, Datadog as high-confidence signals ahead of commit-message tiers. | ✅ |
| G5 | **Multi-tier classification cascade** — override → external-source/issue-type → jira-project → exact/regex/fuzzy rules → LLM, with a rule-based result guaranteed for every commit. | ✅ |
| G6 | **Two-level work taxonomy** — closed 7-variant `TopLevelCategory` + extensible subcategory registry; user-overridable via rules/config. | ✅ |
| G7 | **LLM fallback tier** — OpenRouter (default), AWS Bedrock (feature-gated), and direct Anthropic API; self-enabling `llm:` config section that never stores the key. | ✅ |
| G8 | **Empirical per-commit effort scoring** — deterministic XS–XL T-shirt sizes persisted in `fact_commit_effort`, identical between the Rust path and the parallel bash script. | ✅ |
| G9 | **Per-engineer-per-week quality scoring** — 1–5 score from revert / bugfix / ticket-linkage rates (#377). | ✅ |
| G10 | **DORA delivery metrics** — deployment-frequency, lead-time, change-failure-rate, MTTR from ingested `fact_deployments` / `fact_incidents` and SQL views. | ✅ |
| G11 | **Velocity & activity reporting** — PR cycle time, throughput, revision rate, story points, weighted activity score per engineer per ISO week. | ✅ |
| G12 | **Per-engineer drill-down** — `tga author <email>` assembling effort histogram + PR metrics + commit summary. | ✅ |
| G13 | **Identity resolution** — manual mappings → exact email → GitHub login → Jaro-Winkler fuzzy match → canonical record; CRUD + LLM-assisted alias suggestion. | ✅ |
| G14 | **Reproducible offline output** — single SQLite file (WAL), CSV/JSON/Markdown reports, optional anonymization. | ✅ |
| G15 | **Tag / release-branch reachability** — distinguish deployed-via-release commits from abandoned WIP (`fact_commit_reachability`, #279). | ✅ |
| G16 | **Config/CLI compatibility with `gitflow-analytics`** — same YAML keys and subcommand layout where they still apply. | 🟡 |
| G17 | **Performance regression tracking** — Rust-vs-Python and version-over-version snapshots in `regression-testing/`. | 🟡 |
| G18 | **In-process LLM** — feature-flag `candle`/`llama.cpp` to classify without HTTP. | ⚪ |
| G19 | **Parquet / DuckDB export** — first-class columnar export for analytics pipelines. | ⚪ |

### Non-Goals

| Non-Goal | Rationale |
|---|---|
| Always-on daemon or MCP server | tga is a **batch CLI**; there is no long-running process. Downstream tools read the SQLite file directly. |
| Hosted / multi-tenant SaaS | ELv2-licensed; intended to run on a developer or CI host against local clones. |
| Real-time / streaming analytics | Analysis is run on demand over an ISO-week window; not event-streamed. |
| Code-quality static analysis (complexity, smells) | That is **trusty-analyze**'s job. tga's `complexity` column is an LLM-assigned 1–5 hint, not a static metric. |
| Bitbucket Server / Data Center | Only Bitbucket **Cloud** is supported (ADR-0003). |
| GitLab PR collection | Not implemented — a known parity gap with the Python predecessor. |
| Storing secrets in config | API keys are read from env vars named by config (`api_key_env`), never persisted. |

---

## 3. Target Users / Personas

| Persona | Needs | tga answer |
|---|---|---|
| **Engineering manager** | Weekly per-engineer breakdown of work type, velocity, and effort without nagging the team for status. | `tga analyze --weeks 4` → weekly CSVs + per-author summary + effort histogram. |
| **Individual contributor / tech lead** | A defensible picture of their own contribution mix (feature vs. KTLO vs. maintenance) for reviews. | `tga author <email>` drill-down: effort histogram + PR metrics + commit summary. |
| **CTO / director** | Org-level DORA posture and quality trend to report upward; portfolio coverage confidence. | `tga dora` + `repository_coverage` / `unresolved_authors` fields guard against undercounting. |
| **Platform / data engineer** | A clean, reproducible SQLite/CSV feed into a warehouse (e.g. CTO analytics DuckDB). | Single `tga.db` file + `comprehensive_export_{date}.json`; deterministic effort scores. |

---

## 4. Functional Requirements

### 4.1 Collection (git walk + external fetch) — `tga collect`

Detailed source: [`../requirements/collection.md`](../requirements/collection.md).

| # | Requirement | Status |
|---|---|---|
| C1 | Extract commits via `git2` revwalk (OID traversal, in-process diff); parallel across repos/branches with rayon. | ✅ |
| C2 | Smart branch selection (`smart` / `all` / `main_only`), with `branch_commit_limit`, `max_branches`, `active_days` limits (#331). | ✅ |
| C3 | Per-commit fields: hash, author, timestamp, ISO week, message, files/insertions/deletions, merge flag, `ticketed` / `ticket_id`, `is_revert`. | ✅ |
| C4 | Path-exclusion globs filter diff stats. | ✅ |
| C5 | HTTPS remote fetch with bundled libgit2 TLS (macOS Security / vendored OpenSSL / Schannel); SSH disabled (#337). | ✅ |
| C6 | Ticket-reference extraction (JIRA `[A-Z]{2,10}-\d+` w/ CVE/CWE exclusion, GitHub `#\d+`, ADO `AB#\d+`, Linear); per-provider `ticket_regex` override (#75). | ✅ |
| C7 | GitHub PR + review collection (incremental, open-PR TTL refresh) (#11/#211). | ✅ |
| C8 | Bitbucket Cloud PR collection via `PrProvider` (ADR-0003). | ✅ |
| C9 | Azure DevOps work-item + PR + reviewer collection via WIQL + `workitemsbatch` (#84). | ✅ |
| C10 | JIRA ticket fetch (JQL, story-point field discovery). | ✅ |
| C11 | Identity resolution cascade + canonical records. | ✅ |
| C12 | Tag / release-branch reachability into `fact_commit_reachability` (#279/#290/#303). | ✅ |
| C13 | Per-(repo, ISO-week) collection bookkeeping in `collection_runs`; `--force` re-collects. | ✅ |
| C14 | Always-fetch-before-collect with per-repo error isolation and visible reporting (#334). | ✅ |
| C15 | GitLab PR collection. | 🔵 |

### 4.2 Classification — `tga classify`

Detailed source: [`../requirements/classification.md`](../requirements/classification.md).

| # | Requirement | Status |
|---|---|---|
| L1 | Multi-tier cascade: manual override → external-source/issue-type → jira-project-key → exact (Aho-Corasick) → regex → fuzzy → LLM; rule-based result guaranteed. | ✅ |
| L2 | Two-level taxonomy: closed `TopLevelCategory` (Feature, Bugfix, Ktlo, Integrations, PlatformWork, Content, Maintenance, +Unknown) + extensible subcategory registry. | ✅ |
| L3 | Multi-source signals: JIRA, GitHub Issues, Linear, Shortcut, Confluence, Datadog, resolved + cached per run; confidence varies by signal quality (#270). | ✅ |
| L4 | Per-run external-source cache (in-memory) to avoid duplicate API fetches. | ✅ |
| L5 | LLM tier: batched, circuit-broken, response-cached (90d), confidence-gated; `llm_fallback_threshold` / `llm_fallback_concurrency` (#78/#83). | ✅ |
| L6 | External-source verdicts persist as `external_source` method on the commit (#321/#316). | ✅ |
| L7 | Custom rules from YAML with working priority (#259); `tga rules list/show` to inspect (#209). | ✅ |
| L8 | Coverage metrics + `--validate-coverage` / `--coverage-threshold` gating. | ✅ |
| L9 | WAL checkpoint at exit so a crash mid-classify does not lose work (#298). | ✅ |
| L10 | `external_source` method missing from `classifications` table despite cache hits in some 1.5.2 paths. | 🟡 (#320 open) |

### 4.3 Effort scoring — `tga backfill effort`

Detailed source: [`../research/commit-effort-spec-2026-05-27.md`](../research/commit-effort-spec-2026-05-27.md).

| # | Requirement | Status |
|---|---|---|
| E1 | Deterministic v1 formula `score = α·log₂(LoC+1) + β·log₂(files+1) + δ·tests_factor` (α=1.0, β=1.5, δ=1.0). | ✅ |
| E2 | XS–XL T-shirt bucketing (calibrated against 99 trusty-tools commits, PR #308). | ✅ |
| E3 | Test-LoC detection by path globs; `tests_factor` discount. | ✅ |
| E4 | Persisted to `fact_commit_effort` with `formula_version` for score-evolution tracking. | ✅ |
| E5 | Identical output between the Rust path and the parallel `scripts/compute-effort.sh`. | ✅ |
| E6 | v2 formula adding cyclomatic complexity (γ term). | 🔵 |

### 4.4 Quality scoring — `tga` report path (#377)

| # | Requirement | Status |
|---|---|---|
| Q1 | Per-engineer-per-week 1–5 quality score from `0.35·(1−revert_rate) + 0.40·(1−bugfix_rate) + 0.25·ticket_linkage_rate`. | ✅ |
| Q2 | Negative signals inverted so higher = better, matching the effort scale. | ✅ |
| Q3 | Even 0.20-wide bucketing into integer 1–5. | ✅ |
| Q4 | Shared revert detector (`core/revert.rs`) is the single source of truth for `is_revert` and report-time revert rate (#210/#377). | ✅ |

### 4.5 DORA metrics — `tga deployments` / `tga incidents` / `tga dora`

| # | Requirement | Status |
|---|---|---|
| D1 | Ingest deployment events into `fact_deployments` (idempotent on upstream `deploy_id`) (#212). | ✅ |
| D2 | Ingest incidents into `fact_incidents` with denormalised `mttr_hours` (Datadog/PagerDuty/JIRA SRE) (#213). | ✅ |
| D3 | Derived `deployment_failures` join (failure + recovery commit) (#208). | ✅ |
| D4 | SQL views `v_deployment_frequency`, `v_lead_time`, `v_mttr`, `v_change_failure_rate`. | ✅ |
| D5 | `tga dora` computes & displays the four metrics with Elite/High/Medium/Low performance levels. | ✅ |
| D6 | Datadog deployment-event source feeds classification + DORA (#275/#213). | ✅ |

### 4.6 Reporting — `tga report`

Detailed source: [`../requirements/reporting.md`](../requirements/reporting.md).

| # | Requirement | Status |
|---|---|---|
| R1 | CSV reports (weekly metrics, developer activity, categorization, velocity, DORA, untracked commits, developers). | ✅ |
| R2 | JSON exports (velocity / weekly / quality summaries, comprehensive export). | ✅ |
| R3 | Markdown narrative + qualitative reports via Tera templates. | ✅ |
| R4 | Velocity metrics (cycle time w/ outlier filter, throughput, revision rate, story points/week). | ✅ |
| R5 | Weighted activity scoring (weights sum to 1.0, min-max normalized). | ✅ |
| R6 | Boilerplate / auto-generated-code filter (`flag` / `exclude_from_averages` / `exclude`). | ✅ |
| R7 | Scope/identity-health fields: `repository_coverage`, `unresolved_authors`, `unresolved_author_commits` (#67/#68). | ✅ |
| R8 | Anonymization (`dev_NNN` ids + mapping file). | ✅ |
| R9 | `--author <email>` filter on `tga report` (#324). | ✅ |
| R10 | Per-domain drill-down (`tga author`, #325) — effort histogram + PR metrics + commit summary. | ✅ |

### 4.7 CLI

Detailed source: [`../requirements/cli-commands.md`](../requirements/cli-commands.md).
**Note:** the live command set exceeds that doc — see ARCHITECTURE §CLI surface.

| # | Requirement | Status |
|---|---|---|
| CLI1 | Core stage commands: `analyze`, `collect`, `classify`, `report`. | ✅ |
| CLI2 | Identity: `aliases` (list/add/remove/show/suggest, #347/#348). | ✅ |
| CLI3 | `pr-metrics`, `override`, `install`, `rules`, `backfill`, `author`. | ✅ |
| CLI4 | DORA: `deployments collect`, `incidents collect`, `dora`. | ✅ |
| CLI5 | Uniform `--repo` / `--branch` / `--week` filters across collect/classify/backfill (#332). | ✅ |
| CLI6 | Global `--config`, `--database`, `-v/--log`; `RUST_LOG` precedence. | ✅ |
| CLI7 | Per-subcommand `--help` / examples audit (#333). | ✅ |
| CLI8 | `fetch` and `identities list/merge` standalone subcommands as drafted in `requirements/cli-commands.md`. | 🟡 (folded into `collect` / `aliases`) |

### 4.8 Configuration

Detailed source: [`../requirements/configuration.md`](../requirements/configuration.md).

| # | Requirement | Status |
|---|---|---|
| CFG1 | YAML via serde; `~` expansion; `${VAR}` secret expansion; unknown keys ignored (forward-compatible). | ✅ |
| CFG2 | Top-level `database:` path with `--database > config > tga.db` precedence (#406). | ✅ |
| CFG3 | Top-level `llm:` section (`source` ∈ openrouter/bedrock/anthropic-api, `api_key_env`, `region`, `model`); self-enabling; supersedes legacy `classification.*` fields with deprecation warning (#407/#405). | ✅ |
| CFG4 | Repository / GitHub / Bitbucket / Azure-DevOps / JIRA / Linear / analysis / output / cache / velocity / activity_scoring / boilerplate_filter / quality_report / taxonomy_mapping / teams sections. | ✅ |
| CFG5 | `validate()` cross-field checks (e.g. activity weights sum to 1.0). | ✅ |
| CFG6 | Hard error (non-zero, before any DB write) when LLM is enabled but the named key env var is unset/empty (#405). | ✅ |

### 4.9 Database

Detailed source: [`../requirements/database-schema.md`](../requirements/database-schema.md).

| # | Requirement | Status |
|---|---|---|
| DB1 | Single SQLite file, WAL + `synchronous=NORMAL` + `foreign_keys=ON`; versioned, idempotent migrations (`schema_migrations`). | ✅ |
| DB2 | Core tables: `authors`, `commits`, `classifications`, `files`, `pull_requests`, `pr_reviewers`, `work_items`, `commit_work_items`, `linear_issues`, `classification_overrides`, `collection_runs`, `repository_analysis_status`, `azdo_iterations`. | ✅ |
| DB3 | `fact_*` family: `fact_deployments`, `fact_incidents`, `deployment_failures`, `fact_commit_reachability`, `fact_commit_effort` + DORA views. | ✅ |
| DB4 | Migrations `0001`–`0016` (requirements doc stops at `0013`). | ✅ |
| DB5 | `requirements/database-schema.md` migration list (`0001`–`0013`) is stale vs. on-disk SQL (`0001`–`0016`). | 🟡 (doc drift — see COMPONENTS §Database) |

---

## 5. Success Criteria & Differentiators

- **Reproducibility.** Re-running `tga analyze` over the same window and config
  yields byte-identical effort scores and stable classifications (modulo LLM
  cache expiry). ✅
- **Coverage honesty.** `repository_coverage` and `unresolved_authors` surface
  undercounting and phantom developers instead of silently inflating totals. ✅
- **Work-type accuracy without good commit messages.** Multi-source
  classification reaches through to the ticket behind a commit, so a bare
  `PROJ-1234 update` still classifies correctly. ✅
- **Single-binary, offline.** No daemon, no service, no Python/Node in the hot
  path; one SQLite file is the whole output surface. ✅
- **Speed.** Aspirational 5–10× git-extraction and 4–8× overall-pipeline speedup
  vs. the Python predecessor (`rust-architecture.md`). 🟡 (targets set; tracked in
  `regression-testing/`).

---

## 6. Open Questions / Roadmap

| # | Question | Pointer |
|---|---|---|
| OQ1 | Persist `external_source` method reliably across all 1.5.x cache paths. | #320 (open) |
| OQ2 | v2 effort formula with cyclomatic complexity (γ term). | E6 / `core/effort.rs` header |
| OQ3 | GitLab PR collection for full predecessor parity. | C15 |
| OQ4 | In-process LLM via `candle`/`llama.cpp` (no HTTP). | `rust-architecture.md` Future Improvements |
| OQ5 | Parquet / DuckDB columnar export. | `rust-architecture.md` Future Improvements |
| OQ6 | Reconcile / refresh the `requirements/` docs (taxonomy, DB migration list, tier naming) to match the spec, or formally demote them to "predecessor parity" reference. | DB5 / this PRD |
| OQ7 | gitoxide migration once its diff API stabilizes. | ADR backlog |
