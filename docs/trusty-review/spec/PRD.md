# trusty-review — Product Requirements Document

> **Status:** Living document (v0.1.0 · 2026-06-01)
> **Epic:** [#546](https://github.com/bobmatnyc/trusty-tools/issues/546)
> **Derived from:** spec docs README, 01, 10; open issues #546 #549 #550 #551 #552 #553 #554 #558 #569

---

## 1. What is trusty-review?

**trusty-review** is an AI-assisted GitHub pull-request review service in the `trusty-tools` workspace. It is a ground-up Rust implementation — not a Python port — of the `PRReviewService` previously embedded in the Python `code-intelligence` stack. It orchestrates; it does not index or analyze on its own.

It consumes two sibling daemons — **trusty-search** (semantic code/context retrieval, `:7878`) and **trusty-analyze** (static analysis, `:7879`) — and drives an LLM through a pluggable provider abstraction with co-equal AWS Bedrock and OpenRouter backends. It runs in two modes from the same binary: a **local one-shot CLI** and a **long-lived webhook server**.

The review philosophy is **fail-safe**: the default verdict is `APPROVE` and the bot bears the burden of proof, enforced by a per-finding LLM verification round and a deterministic severity-anchored grade floor.

---

## 2. Goals

| # | Goal |
|---|------|
| G1 | Reproduce the proven review quality behaviors: fail-safe APPROVE default, per-finding verification round, deterministic diff filtering, cross-reference blast-radius search, suppression, dry-run discipline |
| G2 | Be a first-class `trusty-tools` workspace citizen: lib + binary, axum behind `http-server` feature, `thiserror` errors, Why/What/Test doc comments, ≤500-line modules, no global state |
| G3 | Treat the LLM as a runtime-pluggable resource: co-equal Bedrock + OpenRouter behind one trait, models selectable per run and per role |
| G4 | Run as both a CLI one-shot and a webhook server from one binary |
| G5 | Consume trusty-search + trusty-analyze with correct readiness probes and graceful degradation; never use an O(corpus) endpoint as a probe |
| G6 | Treat APEX as a repo in the primary trusty-search index, not a bespoke separate adapter |
| G7 | Encode all 13 hard-won lessons as binding requirements with explicit alarms |

## 3. Non-goals

| # | Non-goal |
|---|----------|
| NG1 | No write access to reviewed repos. No branch creation, file commits, or PR creation. Read + comment only, enforced by a hard-coded push firewall |
| NG2 | Not a code indexer or static analyzer. trusty-review consumes trusty-search/trusty-analyze; it does not embed, index, or parse trees itself |
| NG3 | Not a drop-in API clone of the Python FastAPI routes. The HTTP surface is re-specified for axum; only the webhook contract (HMAC, event filtering) is preserved verbatim |
| NG4 | Not responsible for building/owning the trusty-search index. APEX-as-repo indexing is an upstream (trusty-search) configuration concern |
| NG5 | No auto-fix PR generation in v0.1 |

---

## 4. Feature catalog

Status tags:
- ✅ **Implemented** — built and working in v0.1.0
- 🟡 **Partial** — partly built; usable but incomplete
- 🔵 **Designed-not-built** — spec exists, implementation tracked
- ⚪ **Aspirational** — no committed design

### 4.1 Core review pipeline

| Requirement | Status | Notes |
|-------------|--------|-------|
| Diff loading from GitHub PR | ✅ | `DiffSource::Github`, GitHub App auth + PAT fallback |
| Diff loading from local file | ✅ | `DiffSource::LocalFile` (no GitHub credentials needed) |
| Diff truncation to `MAX_DIFF_CHARS` | ✅ | 60,000 chars in current impl |
| Reviewer LLM call (forced structured JSON output) | ✅ | Bedrock tool-use + OpenRouter equivalent |
| Verdict extraction from LLM response | ✅ | `parse_review_response()` — board-aligned token vocabulary |
| Findings extraction from LLM response | ✅ | JSON `fix_suggestions` block parsing |
| Severity-anchored grade derivation | ✅ | `derive_verdict()` with low-confidence override + severity floor |
| `BLOCK` for `High`-effort findings | ✅ | Compile-break detection via `Effort::High` → BLOCK floor |
| Review log write (JSON + Markdown) | ✅ | `write_review_log()` to `PR_INTELLIGENCE_LOG_DIR` |
| Stdout print of review result | ✅ | `print_review_result()` |
| Fail-safe APPROVE default on pipeline error | ✅ | |
| `Verdict::Unknown` for unassessable diff | ✅ | Preserved through grade derivation |
| 16-stage full pipeline (eligibility, dedup, repo-config, context retrieval, etc.) | 🔵 | Tracked in #552 |
| Per-finding verification round (verifier role) | 🔵 | Tracked in #552 |
| Suppression filtering | 🔵 | Tracked in #552 |
| Relevance gate + dry vs live output paths | 🔵 | Tracked in #552 |

### 4.2 LLM providers

| Requirement | Status | Notes |
|-------------|--------|-------|
| `LlmProvider` trait with `complete()` | ✅ | `llm::LlmProvider` |
| `BedrockProvider` (Converse API) | ✅ | `us.` prefix validated at config time |
| `OpenRouterProvider` (wraps `trusty_common::chat`) | ✅ | |
| Per-run model selection via CLI flags | ✅ | `--reviewer-model`, `--provider` |
| Per-role model selection (reviewer / verifier / summarizer) | ✅ | `RoleModels` config struct |
| Mixed-provider roles (e.g. reviewer Bedrock, summarizer OpenRouter) | ✅ | Per-role provider field in config |
| `bedrock/<id>` / `openrouter/<id>` prefix on model slugs | ✅ | Resolved in `build_provider()` |
| `TRUSTY_REVIEW_*` env-var overrides | ✅ | |
| TOML config file `[models.*]` tables | ✅ | |
| `verification_model_error` alarm on config/lifecycle LLM errors | 🔵 | Alarm infrastructure; tracked in #552 (verification round) |
| Startup model probe | 🔵 | Tracked in #554 |

### 4.3 HTTP server

| Requirement | Status | Notes |
|-------------|--------|-------|
| `GET /health` | ✅ | Version, dry-run flag, dep reachability |
| `GET /status` | ✅ | In-flight count, last error |
| `POST /review` (on-demand synchronous) | ✅ | |
| `POST /pr/github/webhook` (HMAC-validated) | ✅ | |
| `review_requested`-only event filter | ✅ | |
| Trigger classification (manual/auto/force-live) | ✅ | |
| Async dispatch + 200 ack | ✅ | `tokio::spawn` |
| Graceful shutdown on SIGTERM/SIGINT | ✅ | `trusty_common::shutdown_signal()` |
| Default port 7880 | ✅ | `DEFAULT_PORT` |
| Three-layer dedup in server mode | 🔵 | Tracked in #549 + #552 |

### 4.4 CLI

| Subcommand | Status | Notes |
|------------|--------|-------|
| `run <owner> <repo> <pr>` | ✅ | |
| `run --local-diff <path>` | ✅ | |
| `compare` (multi-model table) | ✅ | |
| `serve` | ✅ | |
| `profile` (contributor profiling) | ✅ | |
| `list` / `stats` / `show` / `eval` | 🔵 | Tracked in #553 |

### 4.5 Integrations

| Integration | Status | Notes |
|-------------|--------|-------|
| GitHub App JWT auth + installation token | ✅ | `jsonwebtoken` + reqwest |
| GitHub PAT fallback | ✅ | |
| PR diff / file list / metadata fetch | ✅ | |
| Hard-coded push firewall | ✅ | `assert_no_push_operation()` |
| Webhook HMAC validation | ✅ | |
| trusty-search `HttpSearchClient` | ✅ | |
| trusty-analyze `HttpAnalyzeClient` (two-step probe) | ✅ | `/health` + `/indexes`; never `/quality` |
| JIRA REST client + fallback chain | 🔵 | Tracked in #550 |
| Slack notifications | 🔵 | Tracked in #550 |
| Tracker issue upsert-per-PR | 🔵 | Tracked in #552 |
| Calibration issue (dry-run) | 🔵 | Tracked in #552 |
| GitHub Project v2 (add to project) | 🔵 | Tracked in #550 |
| APEX-as-repo in primary search index | 🔵 | Upstream trusty-search config; tracked in #550 |
| Confluence context | 🔵 | Tracked in #550 |

### 4.6 Diff summarizer (Stage A/B/C)

| Requirement | Status | Notes |
|-------------|--------|-------|
| Noisy-file collapse at diff-fetch stage | 🔵 | Tracked in #551 |
| Stage A — FileFilter (file-level deterministic) | 🔵 | Tracked in #551 |
| Stage B — HunkFilter (hunk-level deterministic) | 🔵 | Tracked in #551 |
| Stage C — HunkClassifier (LLM per-hunk) | 🔵 | Tracked in #551 |

### 4.7 Longitudinal contributor profiles (epic #558)

| Requirement | Status | Notes |
|-------------|--------|-------|
| `ContributorSelector` / identity resolution | ✅ | `profile/selector.rs` |
| Period-batch assembly | ✅ | `profile/batch.rs` |
| Diff sampler | ✅ | `profile/diff_sampler/` |
| `BatchReviewer` (per-period LLM calls) | ✅ | `profile/batch_reviewer.rs`; verification pass TODO |
| `Synthesizer` (longitudinal pattern synthesis) | ✅ | `profile/synthesizer.rs` |
| `Reporter` (JSON + Markdown output) | ✅ | `profile/reporter.rs` |
| `reporter_github.rs` (optional GitHub issue) | ✅ | |
| `profile` CLI subcommand | ✅ | `cli_profile.rs` |
| Per-PR review personalization from contributor profile (#569) | 🔵 | Blocked on full 16-stage pipeline (#552) |

### 4.8 Persistence

| Requirement | Status | Notes |
|-------------|--------|-------|
| Filesystem review log (JSON + Markdown) | ✅ | |
| redb cross-process dedup claim | 🔵 | Tracked in #549 |
| `ReviewLog` trait (pluggable backend) | 🔵 | Tracked in #549 |

### 4.9 Deployment & observability (#554)

| Requirement | Status | Notes |
|-------------|--------|-------|
| Systemd unit for Linux production | 🔵 | Tracked in #554 |
| launchd agent for macOS dev | 🔵 | Tracked in #554 |
| Startup dependency probes | 🔵 | Tracked in #554 |
| `verification_model_error` alarm | 🔵 | Tracked in #554 |
| Metrics emission (verdict distribution, token counts) | 🔵 | Tracked in #554 |
| `/health` dep-reachability details | 🟡 | Basic version/dry-run implemented; dep check fields TBD |

---

## 5. Verdict taxonomy

| Verdict | Meaning | When emitted |
|---------|---------|-------------|
| `APPROVE` | Merge as-is; no concerns | No findings, or only advisory; or all-low-confidence advisory batch |
| `APPROVE*` | Merge; minor advisory notes | Exactly 1 Medium-effort finding (or model proposes but no blocking findings survive) |
| `REQUEST_CHANGES` | Must fix before merge | ≥2 Medium-effort findings at sufficient confidence |
| `BLOCK` | Critical flaw — do not merge | Any High-effort finding (compile-break, auth bypass, data loss) |
| `UNKNOWN` | Could not assess the diff | Diff too truncated or insufficient context |

**Fail-safe default:** `APPROVE`. The bot carries the burden of proof. Pipeline errors fall back to `APPROVE`, never `BLOCK`.

---

## 6. Open issues (unfinished tail as of v0.1.0)

| Issue | Title | Priority |
|-------|-------|----------|
| [#552](https://github.com/bobmatnyc/trusty-tools/issues/552) | Core review pipeline — 16 stages, fail-safe verdict, verification round | High |
| [#549](https://github.com/bobmatnyc/trusty-tools/issues/549) | Data models + persistence — dedup store, review log | High |
| [#554](https://github.com/bobmatnyc/trusty-tools/issues/554) | Deployment, observability, operations — startup probes, metrics, alarms, systemd | Medium |
| [#550](https://github.com/bobmatnyc/trusty-tools/issues/550) | Integration clients — JIRA, Slack, Confluence, GitHub Projects, APEX | Medium |
| [#551](https://github.com/bobmatnyc/trusty-tools/issues/551) | Diff summarizer — Stage A (FileFilter), B (HunkFilter), C (LLM HunkClassifier) | Medium |
| [#553](https://github.com/bobmatnyc/trusty-tools/issues/553) | HTTP + CLI gaps — `list`/`stats`/`show`/`eval` subcommands | Low |
| [#558](https://github.com/bobmatnyc/trusty-tools/issues/558) | Epic: longitudinal per-contributor code review profiles | ✅ Core shipped |
| [#569](https://github.com/bobmatnyc/trusty-tools/issues/569) | Per-PR review personalization from contributor profile | Planned (post-#552) |

The critical path is: **#552** (full pipeline + verification round) → **#549** (persistence) → **#554** (deployment/ops). The diff summarizer (#551) and remaining integrations (#550) can proceed in parallel.

---

## 7. Acceptance checklist (spec conformance gate)

An implementation is spec-conformant only when ALL of the following hold:

- [ ] Verifier-model misconfig produces a `verification_model_error` alarm and (live) refuses to start — never a silent all-approve. (L1)
- [ ] Bedrock model IDs validated for `us.` prefix at config time. (L2)
- [ ] trusty-analyze readiness uses `/health`+`/indexes` only; never `/quality`. (L3)
- [ ] Three dedup layers present: in-process + per-SHA + redb atomic claim + stale purge + release guard. (L4)
- [ ] Verification candidate selection is verdict-conditioned (confidence ≥ 0.50 when REQUEST_CHANGES/BLOCK). (L5)
- [ ] Verification uses the full `MAX_DIFF_CHARS`-bounded diff window. (L6)
- [ ] One tracker issue per PR, upserted (search by title prefix + label). (L7)
- [ ] Dry-run default ON; only `review_requested` dispatches. (L8)
- [ ] All suppression and config lookups fail-open. (L9)
- [ ] trusty-search + LLM are required; trusty-analyze is optional and degrades silently. (L10)
- [ ] Push firewall hard-coded, non-configurable. (L11)
- [ ] Noisy fixtures collapsed at diff-fetch stage; Stage A/B/C diff filtering present. (L12)
- [ ] Cross-reference blast-radius search in parallel context retrieval. (L13)
- [ ] LLM provider trait with co-equal Bedrock + OpenRouter, per-run/per-role model selection. (binding decision #2)
- [ ] APEX treated as a repo in the primary search index. (binding decision #3)
- [ ] Runs as both CLI one-shot and webhook server from one binary. (binding decision #4)

---

## 8. Glossary

| Term | Meaning |
|------|---------|
| **Verdict** | The merge recommendation: `APPROVE`, `APPROVE*`, `REQUEST_CHANGES`, `BLOCK`, or `UNKNOWN` |
| **Finding / FixSuggestion** | A single discrete issue the reviewer raises, with file/line/confidence/effort |
| **Verification round** | A per-finding second-opinion LLM pass (Haiku-tier) that must CONFIRM a blocking finding or it is dropped |
| **Fail-safe / burden of proof** | Default is APPROVE; the bot must prove a problem, not the engineer prove its absence |
| **Severity floor** | Deterministic minimum verdict computed from finding effort distribution |
| **Reviewer / Verifier / Summarizer roles** | The three LLM call-types, each independently model-selectable |
| **Dry-run** | Pipeline runs fully but posts nothing to GitHub; writes a log + calibration issue. Default ON |
| **Tracker issue** | One GitHub issue per PR, upserted on each re-review, carrying the verdict in its title |
| **Suppression** | Mechanism to silence findings by pattern (label-driven or repo-config). Fail-open |
| **Blast radius / cross-reference** | Searching unchanged files that reference symbols a PR deleted/modified |
| **APEX-as-repo** | APEX product specs indexed alongside code repos in trusty-search, queried via the same index |
| **Inference profile prefix** | `us.` prefix required on Bedrock cross-region model IDs (e.g. `us.anthropic.claude-sonnet-4-6`) |
| **Compile-break BLOCK rule** | Any `High`-effort finding (including deleted-symbol compile breaks) triggers BLOCK floor via grade derivation |
