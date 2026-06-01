# trusty-review — Specification Index

**Status:** DRAFT
**Spec version:** 0.1 (2026-05-31)
**Owner:** Bob Matsuoka
**Factual basis:** [`research/source-analysis.md`](research/source-analysis.md) (the grounded analysis of the existing Python PR-review system). Every non-obvious requirement in these docs cites a section of that analysis.

> ⚠️ **This is a specification only.** No implementation code (no `.rs` files, no Cargo crates) is produced or implied to exist by these documents. All requirement IDs (`REV-NNN`) are normative for a future implementation.

---

## One-paragraph overview

**trusty-review** is a new Rust crate (or small family of crates) in the existing `trusty-tools` Cargo workspace that performs AI-assisted GitHub pull-request review. It is a ground-up Rust reimplementation — **not** a Python port — of the Python `PRReviewService` documented in the source analysis. It consumes the two sibling daemons already in the workspace, **trusty-search** (`:7878`, semantic code/context retrieval) and **trusty-analyze** (`:7879`, static-analysis sidecar), and drives an LLM through a single pluggable provider abstraction with **co-equal Bedrock and OpenRouter** backends whose models are selectable **per run and per role** (reviewer / verifier / diff-summarizer). It runs in two deployment modes from the same binary: a **local one-shot CLI** (review a live PR or a local diff) and a **long-lived webhook server** (axum, matching the trusty-search/trusty-analyze pattern). Its review philosophy is **fail-safe**: the default verdict is APPROVE and the bot carries the burden of proof, enforced by a per-finding LLM verification round.

---

## Goals

| # | Goal |
|---|------|
| G1 | Reproduce the proven review *quality behaviors* of the Python service: fail-safe APPROVE default, per-finding verification round, deterministic diff filtering, cross-reference blast-radius search, suppression, dry-run discipline. (source-analysis §2, §3, §12) |
| G2 | Be a first-class `trusty-tools` workspace citizen: lib + binary, axum behind `http-server` feature, `thiserror` errors, Why/What/Test doc comments, ≤500-line modules, no global state. (source-analysis §11.2) |
| G3 | Treat the LLM as a runtime-pluggable resource: **co-equal** Bedrock + OpenRouter behind one trait, models chosen **per run and per role**. (binding decision #2; source-analysis §11.4, §12.1, §12.2) |
| G4 | Run **locally (CLI / one-shot)** or **as a server (webhook)** from one binary. (binding decision #4) |
| G5 | Consume trusty-search + trusty-analyze with correct readiness probes and graceful degradation; never use an O(corpus) endpoint as a probe. (source-analysis §6, §12.3, §12.10) |
| G6 | Treat **APEX as a repo** (indexed alongside code in trusty-search), not as a bespoke separate index. (binding decision #3; source-analysis §7.2, §13) |
| G7 | Encode all 13 hard-won lessons as binding, testable requirements with explicit alarms (esp. `verification_model_error`). (source-analysis §12) |

## Non-goals

| # | Non-goal |
|---|----------|
| NG1 | **No write access to reviewed repos.** No branch creation, file commits, or PR creation. Read + comment only, enforced by a hard-coded push firewall. (source-analysis §4.2, §12.11) |
| NG2 | Not a code indexer or static analyzer. trusty-review *consumes* trusty-search/trusty-analyze; it does not embed, index, or parse trees itself. |
| NG3 | Not a drop-in API clone of the Python FastAPI routes. The HTTP surface is re-specified for axum; only the webhook contract (HMAC, event filtering) is preserved verbatim. |
| NG4 | Not responsible for building/owning the trusty-search index. APEX-as-repo indexing is an upstream (trusty-search) configuration concern; trusty-review only queries it. |
| NG5 | No auto-fix PR generation in v0.1 (the Python `auto_fix_prs` config key is reserved but unimplemented; the push firewall forbids it regardless). |

---

## Glossary

| Term | Meaning |
|------|---------|
| **Verdict** | The merge recommendation: `APPROVE`, `APPROVE*`, `REQUEST_CHANGES`, or `BLOCK`. (source-analysis §2.1) |
| **APPROVE\*** | "Approve with minor notes" — no required-change findings survive. Also the *effective* outcome when all blocking findings are refuted by verification. (source-analysis §2.2) |
| **Finding / FixSuggestion** | A single discrete issue the reviewer raises, with file/line/confidence/effort. (source-analysis §5.2) |
| **Verification round** | A per-finding second-opinion LLM pass (Haiku-tier) that must CONFIRM a blocking finding or it is dropped. (source-analysis §2.2) |
| **Fail-safe / burden of proof** | Default is APPROVE; the bot must prove a problem, not the engineer prove its absence. (source-analysis §2.1) |
| **Reviewer / Verifier / Summarizer roles** | The three LLM call-types, each independently model-selectable. (binding decision #2) |
| **Dry-run** | Pipeline runs fully but posts nothing to GitHub; writes a log + calibration issue. Default ON. (source-analysis §10.1, §12.8) |
| **Tracker issue** | One GitHub issue per PR, upserted on each re-review, carrying the verdict in its title. (source-analysis §4.3, §12.7) |
| **Suppression** | Mechanism to silence findings by pattern (label-driven or repo-config). Fail-open. (source-analysis §4.4, §8.2, §12.9) |
| **Blast radius / cross-reference** | Searching unchanged files that reference symbols a PR deleted/modified. (source-analysis §12.13) |
| **APEX-as-repo** | APEX product specs indexed alongside code repos in trusty-search and queried via the same index, not a separate adapter. (source-analysis §7.2, §13) |
| **trusty-search** | Sibling daemon, `:7878`, hybrid BM25+vector search. **Required** dependency. |
| **trusty-analyze** | Sibling daemon, `:7879`, static analysis (hotspots, smells). **Optional** dependency (graceful degradation). |

---

## How the documents fit together

```
README.md  ......................  you are here (index, goals, glossary)
01-architecture.md  .............  crate layout, dependency topology, local vs server
02-pr-review-pipeline.md  .......  the ordered review stages + verdict/verification policy
03-diff-summarizer.md  ..........  standalone diff analyzer (Stage A/B/C) spec
04-llm-providers.md  ............  provider trait, Bedrock+OpenRouter, per-role models
05-integrations.md  .............  GitHub / JIRA / APEX / trusty-search / trusty-analyze / Slack
06-configuration.md  ............  global config + env + per-repo .github/code-intelligence.yml
07-data-models.md  ..............  review result, fix-suggestion, enums, persistence choice
08-interfaces.md  ...............  HTTP API (webhook) + CLI surface
09-deployment-operations.md  ....  local vs server runtime, systemd, health, observability
10-lessons-and-rationale.md  ....  13 lessons → binding requirements + NEW-vs-today delta
```

Reading order for an implementer: `01` → `04` → `02` → `03` → `05` → `06`/`07` → `08` → `09`, with `10` as the cross-cutting requirements ledger to satisfy throughout. Each document is independently navigable and cross-links to the others.

---

## Requirement ID scheme

Requirements use the prefix `REV-` followed by a zero-padded number. The hundreds digit groups by document:

| Range | Document |
|-------|----------|
| REV-0xx | Architecture (01) |
| REV-1xx | Pipeline (02) |
| REV-2xx | Diff summarizer (03) |
| REV-3xx | LLM providers (04) |
| REV-4xx | Integrations (05) |
| REV-5xx | Configuration (06) |
| REV-6xx | Data models (07) |
| REV-7xx | Interfaces (08) |
| REV-8xx | Deployment/operations (09) |
| REV-9xx | Lessons/rationale (10) |

"**Rationale (lesson learned)**" callouts trace a requirement back to a numbered lesson in source-analysis §12.
