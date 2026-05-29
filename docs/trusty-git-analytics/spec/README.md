# trusty-git-analytics (`tga`) — Specification Set

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-05-29
> **Derived from:** existing `requirements/` docs + code/tickets reconciliation

This directory holds the canonical product and engineering specification for the
`tga` crate (package name `tga`, directory `crates/trusty-git-analytics/`,
version `2.3.0`). It is the single authoritative reference for *what tga is meant
to be*, *what it is today*, and *what gaps remain*.

## What is trusty-git-analytics?

`tga` is a **developer-productivity analytics CLI and embedded SQLite database**.
It walks local git repositories, correlates each commit with external systems
(GitHub / Bitbucket / Azure DevOps PRs; JIRA / Linear / Shortcut / GitHub-Issues
tickets), classifies every commit into a two-level work taxonomy via a
multi-tier cascade, scores per-commit *effort* and per-engineer *quality*, tracks
DORA delivery metrics from ingested deployment and incident events, and emits
per-author / per-week / velocity / DORA / quality reports as CSV, JSON, and
Markdown. It originated as a Rust port of the Python `gitflow-analytics` tool but
has since grown capabilities (effort scoring, DORA fact tables, a two-level
taxonomy, per-engineer drill-downs) that the predecessor never had. Everything is
local: a single `tga` binary, one SQLite file (`tga.db`, WAL mode), no daemon and
no service dependency.

## Documents in this set

| Document | Read it when you want to know… |
|---|---|
| **[PRD.md](./PRD.md)** | The product: vision & mission, goals/non-goals, personas (eng managers, ICs, CTO), and the full functional-requirement catalogue grouped by capability (collection, classification, effort scoring, quality scoring, DORA, reporting, CLI, configuration, database) tagged by implementation status. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: the `collect → classify → score → aggregate → report` pipeline, the single SQLite schema (with the `fact_*` family that the requirements docs predate), the LLM config layer (`llm:` section, OpenRouter / Bedrock / Anthropic per #405/#406/#407), and the CLI surface. Includes the source-module map with `src/` citations. Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-subsystem specs: collector, classifier (tiers + sources), effort scorer, quality scorer, reporter/aggregator + drill-down, database/migrations, CLI, and config/LLM. Each states responsibility, key types (with `src/` paths), current state, and known gaps. Start here for *the detail of one subsystem*. |

## Reading order

1. **New to tga?** PRD → ARCHITECTURE → COMPONENTS.
2. **Implementing a feature?** Jump to the relevant COMPONENTS section, then
   cross-check the pipeline in ARCHITECTURE.
3. **Evaluating product direction?** PRD vision + success criteria, then the gap
   callouts throughout COMPONENTS.

## Status legend (used throughout this set)

Every requirement and component is framed as **Vision / Current / Gap** and
tagged inline with one of:

| Tag | Meaning |
|---|---|
| ✅ **Implemented** | Built and working today. |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats. |
| 🔵 **Designed-not-built** | Design exists (types, scaffolding, or plan) but no working path. |
| ⚪ **Aspirational** | North-star intent; no design committed yet. |

## Relationship to the `requirements/` docs

This `spec/` set **supersedes and sits above** the nine detailed documents in
[`../requirements/`](../requirements/) (`overview.md`, `collection.md`,
`classification.md`, `reporting.md`, `cli-commands.md`, `configuration.md`,
`database-schema.md`, `rust-architecture.md`, `index.md`). Those remain as the
**detailed source material** — the field-by-field config schema, the full
migration list, the rule-tier tables — and are linked from the relevant spec
sections. Where the two disagree, **this spec set is authoritative** because it
was reconciled against the `crates/trusty-git-analytics/src/` tree and the issue
backlog as of the review date. The most material reconciliations:

- **Taxonomy.** `requirements/classification.md` describes a flat 19-value
  `ChangeType` enum. The code implements a **two-level taxonomy** — a closed
  7-variant `TopLevelCategory` (+ `Unknown`) plus an extensible subcategory
  registry (`TaxonomyRegistry`). The 19 names survive as *subcategories* that
  roll up to the seven top-levels. See COMPONENTS §Classifier.
- **DORA.** The requirements treat DORA purely as report-time arithmetic. The
  code ships a **full DORA subsystem**: `fact_deployments`, `fact_incidents`,
  `deployment_failures`, four SQL views, and `tga deployments` / `tga incidents`
  / `tga dora` commands (migration `0014`, issues #207/#208/#212/#213).
- **Effort & quality scoring.** Neither appears in the requirements. The code has
  `core/effort.rs` + `fact_commit_effort` (`tga backfill effort`, PR #308) and
  `core/quality.rs` (#377).
- **Reachability.** `fact_commit_reachability` (migration `0015`, #279) tracks
  tag/release-branch reachability — absent from the requirements.
- **Tier naming.** The classify cascade modules are named `exact / regex / fuzzy
  / llm` with `override / issue_type / jira_project / weighted_sum / bedrock`
  helpers — not the `Tier 0 / 1.5 / 3` numbering used in
  `requirements/classification.md`.
- **DB table names.** Live tables are `commits` / `collection_runs`, not the
  predecessor's `cached_commits` / `weekly_fetch_status`. Migrations run to
  `0016`, past the `0013` ceiling documented in `requirements/database-schema.md`.

## Related documentation

This `spec/` set is the *what/why/gap* layer. The point-in-time and operational
docs live alongside it:

- **[../requirements/](../requirements/)** — the detailed source specification
  (config schema, DB schema, CLI flags, classification cascade).
- **[../decisions/](../decisions/)** — crate-specific ADRs (SQLite tuning,
  performance hotspots, Bitbucket PR provider).
- **[../developer/](../developer/)** — contributor architecture, developer guide,
  configuration reference, migration-from-Python.
- **[../user/](../user/)** — the end-user CLI guide.
- **[../research/](../research/)** — dated investigations (commit-effort spec,
  per-engineer drill-down).
- **[../regression-testing/](../regression-testing/)** — versioned performance
  snapshots and the Rust-vs-Python comparison.
- **[crates/trusty-git-analytics/README.md](../../../crates/trusty-git-analytics/README.md)**
  — in-crate quick-start and CLI catalogue.

## Provenance & maintenance

These documents were derived by reconciling the existing `requirements/` set
against an audit of the `crates/trusty-git-analytics/src/` tree (the single `tga`
crate with `core` / `collect` / `classify` / `report` / `commands` modules as of
v2.3.0), the crate `Cargo.toml`, and the open/closed issue backlog (notably the
DORA work #207/#208/#212/#213, the effort spec PR #308, the quality metric #377,
the reachability work #279, and the LLM-config trio #405/#406/#407). When the
code changes materially, update the relevant document and bump the *Last
reviewed* date. Source-path citations reflect the layout at the time of review.
