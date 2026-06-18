# trusty-review — Product Requirements Document

> **Status:** Living document (reconciled to `main` · 2026-06-18; v0.1.0 spec markers updated — see §9 and README.md note)
> **Epic:** [#546](https://github.com/bobmatnyc/trusty-tools/issues/546)
> **Derived from:** spec docs README, 01, 10; open issues #546 #549 #550 #551 #552 #553 #554 #558 #569 #584 #586 #680 #1241 #1413–#1423

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
| Diff truncation to `MAX_DIFF_CHARS` | ✅ | 160,000 chars (raised from 60 K in #624/#627) |
| Reviewer LLM call (forced structured JSON output) | ✅ | Bedrock tool-use + OpenRouter equivalent |
| Verdict extraction from LLM response | ✅ | `parse_review_response()` — board-aligned token vocabulary |
| Findings extraction from LLM response | ✅ | JSON `fix_suggestions` block parsing |
| Severity-anchored grade derivation | ✅ | `derive_verdict()` with low-confidence override + severity floor |
| `BLOCK` for `High`-effort findings | ✅ | Compile-break detection via `Effort::High` → BLOCK floor |
| **Letter grade (A+…F) + 5-verdict model** | ✅ | `APPROVE`/`APPROVE*`/`REQUEST_CHANGES`/`BLOCK`/`UNKNOWN`; severity-anchored verdict floor |
| **Fail-closed on parse/truncation** | ✅ | #1241: truncation or parse failure → `UNKNOWN`, not silent `APPROVE` |
| Review log write (JSON + Markdown) | ✅ | `write_review_log()` to `PR_INTELLIGENCE_LOG_DIR` |
| Stdout print of review result | ✅ | `print_review_result()` |
| **Review footer** (model · tokens · est. cost) | ✅ | #728: appended to every posted review body |
| Fail-safe APPROVE default on pipeline error | ✅ | |
| `Verdict::Unknown` for unassessable diff | ✅ | Preserved through grade derivation |
| **Per-finding LLM verification round** | ✅ | `pipeline/verify.rs`; verifier-role model; `buffer_unordered(4)` fan-out |
| **Verifier liveness gate + `verification_model_error` alarm** | ✅ | Config/lifecycle LLM errors → ERROR alarm; startup model probe |
| Suppression filtering | 🔵 | Tracked in #552 (per-repo config suppression) |
| 16-stage full pipeline (eligibility, repo-config, copilot-mode detection, etc.) | 🔵 | Tracked in #552 |
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
| **Live GitHub PR review comment posting** | ✅ | Posts review body + verdict to PR via GitHub API when `dry_run=false` |
| **MCP server** (`review_pr`/`review_diff`/`review_health` tools) | ✅ | `src/mcp/tools.rs`; all three tools route through `run_review()` |
| **Three-layer dedup** (in-process + per-SHA + redb) | ✅ | `store/dedup.rs` used by `pipeline/runner.rs`; cross-process safe |

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
| trusty-search `HttpSearchClient` | ✅ | RAG context retrieval |
| trusty-analyze `HttpAnalyzeClient` (two-step probe) | ✅ | `/health` + `/indexes`; never `/quality` |
| **JIRA context source** | ✅ | `integrations/context/jira.rs`; REST + keyword fallback chain; fail-open |
| **Confluence context source** | ✅ | `integrations/context/confluence.rs`; fail-open |
| **GitHub Issues context source** | ✅ | `integrations/context/github_issues.rs`; fail-open |
| **Intent/method-conformance context source** | ✅ | `integrations/context/conformance.rs`; checks PR intent vs method spec |
| **APEX indexed path** | ✅ | `integrations/apex_context.rs`; APEX queried via trusty-search `apex_index` config |
| **Voice packages + universal principles** (3-layer prompt composition) | ✅ | Reviewer system prompt includes voice layer + universal engineering principles |
| JIRA REST client (full upsert/tracker) | 🔵 | Tracked in #550 |
| Slack notifications | 🔵 | Tracked in #550 |
| Tracker issue upsert-per-PR | 🔵 | Tracked in #552 |
| Calibration issue (dry-run) | 🔵 | Tracked in #552 |
| GitHub Project v2 (add to project) | 🔵 | Tracked in #550 |

### 4.6 Diff summarizer (Stage A/B/C) and map-reduce

| Requirement | Status | Notes |
|-------------|--------|-------|
| Noisy-file collapse at diff-fetch stage | ✅ | `NOISY_FILE_PATTERNS` in `pipeline/diff.rs`; Stage A/B inline |
| Stage A — FileFilter (file-level deterministic) | ✅ | `pipeline/diff_analyzer/file_filter.rs` + tests |
| Stage B — HunkFilter (hunk-level deterministic) | ✅ | `pipeline/diff_analyzer/hunk_filter.rs` + tests |
| Stage C — HunkClassifier (LLM per-hunk) | ✅ | `pipeline/diff_analyzer/hunk_classifier.rs` + tests |
| **Map-reduce config + mode selector** (Phase 1 of #680) | ✅ | `config/mapreduce.rs`; `TRUSTY_REVIEW_MAP_MODE ∈ {auto,always,never}`; `select_review_mode` |
| **Per-file diff splitter** `MapUnit` (Phase 2 of #680) | ✅ | `pipeline/mapreduce/split.rs`; hunk sub-chunking, rename/binary/deleted handling |
| Map fan-out: bounded-parallel per-file LLM reviews (Phase 3 of #680) | 🔵 | Tracked in #680 |
| Reduce stage: dedup + precedence + synthesis (Phase 4 of #680) | 🔵 | Tracked in #680 |
| Wire map-reduce into `run_review` + `MapReduceStats` (Phase 5 of #680) | 🔵 | Tracked in #680 |
| Enable auto map-reduce by default (Phase 6 of #680) | 🔵 | Tracked in #680 |

### 4.7 Longitudinal contributor profiles (epic #558)

| Requirement | Status | Notes |
|-------------|--------|-------|
| `ContributorSelector` / identity resolution | ✅ | `profile/selector.rs` |
| Period-batch assembly | ✅ | `profile/batch.rs` |
| Diff sampler | ✅ | `profile/diff_sampler/` |
| `BatchReviewer` (per-period LLM calls) | ✅ | `profile/batch_reviewer.rs` |
| `Synthesizer` (longitudinal pattern synthesis) | ✅ | `profile/synthesizer.rs` |
| `Reporter` (JSON + Markdown output) | ✅ | `profile/reporter.rs` |
| `reporter_github.rs` (optional GitHub issue) | ✅ | |
| **`profile` CLI subcommand** | ✅ | `cli_profile.rs`; full longitudinal contributor pipeline |
| Per-PR review personalization from contributor profile (#569) | 🔵 | Blocked on full 16-stage pipeline (#552) |

### 4.8 Persistence

| Requirement | Status | Notes |
|-------------|--------|-------|
| Filesystem review log (JSON + Markdown) | ✅ | |
| **Three-layer dedup** (in-process + per-SHA + redb cross-process) | ✅ | `store/dedup.rs`; atomic insert-or-fail; stale claim purge; Drop-guard release |
| `ReviewLog` trait (pluggable backend) | 🔵 | Tracked in #549 |

### 4.9 Deployment & observability (#554)

| Requirement | Status | Notes |
|-------------|--------|-------|
| Systemd unit for Linux production | 🔵 | Tracked in #554 |
| launchd agent for macOS dev | 🔵 | Tracked in #554 |
| Startup dependency probes | 🔵 | Tracked in #554 |
| **`verification_model_error` alarm** | ✅ | Fires at ERROR level on config/lifecycle LLM errors; startup model probe active |
| Metrics emission (verdict distribution, token counts) | 🔵 | Tracked in #554 (verdict-distribution metrics: #554) |
| `/health` dep-reachability details | 🟡 | Basic version/dry-run + dep reachability shape implemented; full structured dep fields TBD |

### 4.10 Coverage policy and thresholds

| Requirement | Status | Notes |
|-------------|--------|-------|
| **Coverage policy** | 🟡 | Off by default; `suppress_advisory_reviews` and `min_findings_to_post` constants exist; TOML per-repo config pending #586 |
| **Configurable thresholds** | 🟡 | Confidence constants in `config/constants.rs` (#584); TOML-per-repo config pending #586 |
| Suppression / per-repo config file | 🔵 | Tracked in #584; `.github/code-intelligence.yml` schema specified, not yet fetched in pipeline |

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

## 6. Open issues (unfinished tail as of 2026-06-18)

| Issue | Title | Priority |
|-------|-------|----------|
| [#552](https://github.com/bobmatnyc/trusty-tools/issues/552) | Core review pipeline — 16 stages (eligibility, repo-config, copilot-mode detection, suppression, relevance gate) | High |
| [#549](https://github.com/bobmatnyc/trusty-tools/issues/549) | Persistence — `ReviewLog` trait (pluggable backend) | High |
| [#554](https://github.com/bobmatnyc/trusty-tools/issues/554) | Deployment, observability, operations — startup probes, verdict-distribution metrics, systemd | Medium |
| [#550](https://github.com/bobmatnyc/trusty-tools/issues/550) | Remaining integration sinks — Slack notifications, GitHub Projects v2, tracker issue upsert-per-PR | Medium |
| [#584](https://github.com/bobmatnyc/trusty-tools/issues/584) | Suppression / per-repo config — `.github/code-intelligence.yml` fetch + per-repo threshold overrides | Medium |
| [#586](https://github.com/bobmatnyc/trusty-tools/issues/586) | Configurable threshold TOML — expose `config/constants.rs` values as TOML config keys | Medium |
| [#680](https://github.com/bobmatnyc/trusty-tools/issues/680) | Map-reduce review for large PRs — Phases 3–6 (map fan-out, reduce, wire-in, enable by default) | Medium |
| [#553](https://github.com/bobmatnyc/trusty-tools/issues/553) | HTTP + CLI gaps — `list`/`stats`/`show`/`eval` subcommands | Low |
| [#558](https://github.com/bobmatnyc/trusty-tools/issues/558) | Epic: longitudinal per-contributor code review profiles | ✅ Core shipped |
| [#569](https://github.com/bobmatnyc/trusty-tools/issues/569) | Per-PR review personalization from contributor profile | Planned (post-#552) |
| [#1242](https://github.com/bobmatnyc/trusty-tools/issues/1242) | OpenRouter Zero Data Retention (ZDR) opt-in | Planned |
| [#1413](https://github.com/bobmatnyc/trusty-tools/issues/1413) | Epic: best-practice & Duetto-alignment review improvements | Planned (see §9) |
| [#1414](https://github.com/bobmatnyc/trusty-tools/issues/1414) | Inline per-line GitHub review comments | P1 |
| [#1415](https://github.com/bobmatnyc/trusty-tools/issues/1415) | One-click `suggestion` blocks in review comments | P1 |
| [#1416](https://github.com/bobmatnyc/trusty-tools/issues/1416) | Consequence/failure-mechanism + uncertainty signaling | P1 |
| [#1417](https://github.com/bobmatnyc/trusty-tools/issues/1417) | Conventional-Comments labels + praise-first verdict + bias-to-merge | P1 |
| [#1418](https://github.com/bobmatnyc/trusty-tools/issues/1418) | Test-plan & AC conformance check | P1 |
| [#1419](https://github.com/bobmatnyc/trusty-tools/issues/1419) | Spec-grounding density + external linked-spec fetch by ticket key | P1 |
| [#1420](https://github.com/bobmatnyc/trusty-tools/issues/1420) | "What NOT to flag" guardrails + nit cap/rollup, suppress nits on re-review | P2 |
| [#1421](https://github.com/bobmatnyc/trusty-tools/issues/1421) | Review outcome feedback loop (reactions/acted-on/quick-merge → auto-suppression) | P2 |
| [#1422](https://github.com/bobmatnyc/trusty-tools/issues/1422) | Calibration harness recall/precision vs positive-signal corpus | P2 |
| [#1423](https://github.com/bobmatnyc/trusty-tools/issues/1423) | Prior-PR / change-history context source | P3 |

The critical path is: **#552** (full 16-stage pipeline) → **#549** (persistence layer) → **#554** (deployment/ops). Map-reduce (#680), best-practice alignment (#1413–#1423), and remaining integrations (#550) can proceed in parallel.

---

## 7. Acceptance checklist (spec conformance gate)

> _Section 9 (Best-practice & Duetto-alignment requirements) was added 2026-06-18 and adds additional acceptance items at the bottom of §7._

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

---

## 9. Best-practice & Duetto-alignment requirements (epic #1413)

> **Added 2026-06-18.** Requirements derived from analysis of Duetto human PR reviews
> that earned positive author feedback, combined with general best practices from
> Google eng-practices, Conventional Comments, Cloudflare/Greptile/Qodo, and SmartBear.
>
> See COMPONENTS.md §13 for subsystem mapping. Cross-references to #584 (suppression/per-repo
> config), #586 (configurable thresholds), and #680 (map-reduce) are noted inline where
> those features are prerequisites or close complements.

---

### 9.1 Inline per-line GitHub review comments — #1414 (P1, MUST)

**Requirement:** When posting a review to GitHub, each finding with a `file` + `line`
MUST be posted as an **inline pull-request review comment** at the exact file/line
using the GitHub `POST /repos/{owner}/{repo}/pulls/{pull_number}/reviews` API with
`comments[]` entries, not as a monolithic review body. Findings without a file/line
SHOULD be aggregated into the review summary body.

**Rationale:** Authors navigate findings most efficiently when comments appear inline
in the diff view. Duetto human reviews that earned the most positive feedback
universally used inline comments; a wall-of-text review body in isolation consistently
drew requests to "please annotate the specific lines." The GitHub PR review API
supports batching all inline comments into one API call alongside the overall verdict.

**Forward reference:** #1414. Depends on live posting (already ✅). Compatible with
map-reduce (#680) — per-file findings already carry `file`+`line`.

---

### 9.2 One-click `suggestion` blocks — #1415 (P1, SHOULD)

**Requirement:** Where a finding's `suggestion` field contains a complete, actionable
replacement for the flagged code, the posted inline comment SHOULD wrap that
replacement in a GitHub `suggestion` fenced code block:

````
```suggestion
<replacement code>
```
````

This lets the author apply the fix with one click without leaving the PR interface.
The LLM prompt MUST instruct the model to emit `suggestion` field content as a
complete drop-in replacement (not an explanation) when the fix is a single contiguous
code block. Suggestions spanning multiple non-contiguous lines SHOULD be omitted
from the `suggestion` block to avoid invalid diff application.

**Rationale:** One-click suggestions are the highest-signal feature in GitHub's
review UX and are explicitly recommended in Conventional Comments as the fastest
path to acceptance. Duetto reviews that included them had the shortest time-to-merge
for the flagged fixes.

**Forward reference:** #1415. Requires inline comment posting (#1414).

---

### 9.3 Consequence/failure-mechanism + uncertainty signaling — #1416 (P1, MUST)

**Requirement:**

1. Every `High`- or `Medium`-effort finding MUST include a `consequence` field (or
   embed the consequence inline in `description`) that names the concrete failure
   mechanism: what breaks, how it manifests, at what scale or frequency. Vague
   findings such as "this could cause issues" are not acceptable.

2. When the model is uncertain about a finding (confidence < `BLOCK_ISSUE_MIN_CONFIDENCE`
   = 0.75), the finding's `description` MUST include an explicit hedge, e.g.
   *"I may be missing context about…"* or *"This could be intentional if…"* The
   reviewer system prompt MUST reinforce this contract.

**Rationale:** The most common negative feedback on AI reviews in Duetto audits was
"it flags things but doesn't tell me what actually breaks." Consequence-first framing
converts a vague concern into an actionable decision. Explicit uncertainty signaling
avoids the false authority problem: authors distrust confident-sounding findings that
turn out to be speculative, which erodes trust in real findings over time.

**Forward reference:** #1416. Requires reviewer prompt update + Finding model `consequence`
field addition.

---

### 9.4 Conventional-Comments severity labels + praise-first verdict, bias-to-merge — #1417 (P1, MUST)

**Requirement:**

1. Posted review comments MUST be prefixed with a Conventional Comments label that
   matches the finding's `effort` level:
   - `Effort::High` → `**blocker:**`
   - `Effort::Medium` → `**issue:**` (or `**suggestion:**` if the fix is optional)
   - `Effort::Low` → `**nit:**`
   Advisory/praise findings → `**praise:**` or `**note:`**

2. The overall review body MUST open with praise or acknowledgment of what the PR
   does well before stating concerns. The reviewer system prompt MUST include this
   instruction.

3. The prompt rubric MUST encode a **bias-to-merge**: the reviewer default posture is
   "approve unless there is a concrete, well-evidenced reason not to." Speculative
   concerns that cannot be tied to a specific failure mechanism MUST be demoted to
   `Effort::Low` / `nit` or omitted.

**Rationale:** Conventional Comments is the industry standard (adopted by GitLab,
Shopify, Cloudflare internal tools) and eliminates ambiguity about whether a comment
is blocking. Praise-first framing mirrors how Duetto's highest-rated human reviews are
structured and reduces author defensiveness. Bias-to-merge is the formal statement of
the existing fail-safe posture (§6) at the prompt level.

**Forward reference:** #1417. Requires reviewer prompt update + comment formatting layer.
Cross-references §6 (fail-safe), §4.1 verdict taxonomy.

---

### 9.5 Test-plan & AC conformance — #1418 (P1, MUST; advisory when absent)

**Requirement:**

1. When a PR description contains a test plan (recognizable by a `## Test plan`,
   `**Testing:**`, or checklist section), the review MUST include a finding that
   evaluates whether the diff's test coverage plausibly satisfies the stated plan.
   If coverage is insufficient, this is an `Effort::Medium` finding minimum.

2. When a PR links to a JIRA ticket (via `integrations/context/jira.rs`) and the
   ticket contains Acceptance Criteria, the review MUST surface a finding that
   evaluates whether the changed code satisfies those criteria. Missing coverage
   of a stated AC is at minimum `Effort::Medium`.

3. When no test plan or AC is present, the review SHOULD note this as a `nit`-level
   advisory (not a blocker) and suggest the author add one. This is advisory, not
   blocking — enforcement is a team process decision.

**Rationale:** The gap between "code looks right" and "code satisfies requirements"
is the most common source of post-merge rework in Duetto's retrospectives.
Test-plan conformance was the single requirement most often cited in positive
review feedback: "caught that we missed the edge case in AC-3."

**Forward reference:** #1418. Depends on JIRA context source (already ✅) and PR
metadata fetch (already ✅). JIRA AC extraction requires AC-structured ticket parsing
(new, scoped to #1418).

---

### 9.6 Spec-grounding density + external linked-spec fetch by ticket key — #1419 (P1, SHOULD/MUST)

**Requirement:**

1. (SHOULD) When a finding references a design rule, architectural constraint, or
   engineering principle, it SHOULD cite a specific, linkable reference: a spec
   section, a CLAUDE.md rule, a ticket number, or a named best-practice document.
   Ungrounded assertions ("this violates SOLID principles") without citation are
   NOT acceptable for `Effort::Medium` or higher.

2. (MUST when configured) When `TRUSTY_REVIEW_SPEC_FETCH=true` and a JIRA ticket
   key is extracted from the PR, the context orchestrator (`integrations/context/`)
   MUST attempt to fetch the linked spec or design doc from Confluence
   (`integrations/context/confluence.rs`, already ✅) and include it as a
   named context block in the reviewer prompt. If the fetch fails, it MUST fail-open
   (no spec context, no review block).

**Rationale:** Grounded findings are 3× more actionable than assertion-only findings
in Duetto review audits. Authors can verify or contest a spec citation; they cannot
engage with an ungrounded assertion. Confluence spec fetch (already implemented as a
context source) unlocks automatic spec grounding without manual copy-paste.

**Forward reference:** #1419. Depends on Confluence context source (already ✅) and
JIRA context source (already ✅). Cross-references #584 (per-repo spec config).

---

### 9.7 "What NOT to flag" guardrails + nit cap/rollup, suppress nits on re-review — #1420 (P2, MUST)

**Requirement:**

1. The reviewer system prompt MUST include an explicit "do NOT flag" section
   enumerating categories of false-positive-prone findings that should be suppressed:
   - Style preferences covered by the project's configured linter/formatter
   - Import ordering or whitespace that a formatter enforces automatically
   - Dependency version choices that are not security-relevant
   - Test names, variable names, or comment style below `Effort::High`
   - Findings the model rates `confidence < 0.50` (already filtered by
     `FIX_ISSUE_MIN_CONFIDENCE` but the prompt must reinforce this)

2. A single review MUST NOT surface more than `TRUSTY_REVIEW_MAX_NITS` (default `5`,
   configurable via `#586`) `Effort::Low` findings. Additional low-effort findings
   MUST be collapsed into a single rollup comment: *"N additional style/nit findings
   suppressed — run the linter locally."*

3. On a **re-review** of a PR (same `(owner, repo, pr_number)`, new head SHA),
   findings that were already posted on a prior review head SHA MUST be suppressed
   unless the relevant code changed in the new diff. This prevents "nit spam" on
   iterative PRs. Cross-reference #584 (per-repo suppression).

**Rationale:** Over-flagging is the #1 reason authors disable or ignore AI review
tools (SmartBear developer survey, Qodo eng-practices whitepaper). Nit rollup and
re-review suppression directly address the two most common "reviewer fatigue"
complaints in Duetto's internal tooling retrospectives.

**Forward reference:** #1420. Requires reviewer prompt update, `MAX_NITS` constant
in `config/constants.rs`, and re-review suppression (depends on dedup store,
already ✅, and prior-review log read). Cross-references #584, #586.

---

### 9.8 Review outcome feedback loop — #1421 (P2, SHOULD)

**Requirement:**

When a posted review comment receives a 👍 reaction (via GitHub Reactions API) OR
the PR author explicitly closes/resolves the comment, OR the PR merges within
`TRUSTY_REVIEW_QUICK_MERGE_HOURS` (default `4`) of posting with only `APPROVE`
or `APPROVE*` verdict, the system SHOULD record this as a **positive signal** for
that review. When a comment receives a 👎 reaction or is explicitly marked
"outdated/irrelevant," it SHOULD record a **negative signal**.

Accumulated positive/negative signals per `(finding_kind, file_pattern)` SHOULD
feed an auto-suppression heuristic: a finding category with a negative signal rate
> `TRUSTY_REVIEW_AUTO_SUPPRESS_THRESHOLD` (default `0.70`) over the last
`N_SIGNAL_WINDOW` (default `20`) signals SHOULD be auto-suppressed (downgraded to
`Effort::Low`, subject to nit cap) without human-authored suppression config.

**Rationale:** Outcome signals close the loop between review quality and review
behavior without requiring maintainers to manually curate suppression lists.
This is the same pattern used in Greptile's adaptive review and is the organic
evolution of the Python predecessor's calibration issue (#585) toward a
fully-automated feedback loop.

**Forward reference:** #1421. Depends on GitHub Reactions API access (requires
`integrations/github/` extension). Complements #584 (manual suppression) and
#1422 (calibration harness).

---

### 9.9 Calibration harness recall/precision vs positive-signal corpus — #1422 (P2, SHOULD)

**Requirement:**

A calibration test harness SHOULD be implemented (`regression-testing/`) that:
1. Replays a corpus of PRs from GitHub org Project #19 (Duetto's
   positive-signal PR set — PRs that earned explicit positive author feedback)
   through the review pipeline in dry-run mode.
2. Computes **recall** (fraction of positive-signal findings that the bot also
   surfaced, at any confidence threshold) and **precision** (fraction of bot
   findings that appear in the positive-signal set or were acted on by the author).
3. Emits a regression report comparing current recall/precision against the
   baseline snapshot. A regression of >10% on either metric on the corpus SHOULD
   block the PR via CI.

**Rationale:** Without a ground-truth corpus, tuning the grade floor, nit cap,
confidence thresholds (#586), and "what NOT to flag" guardrails (#1420) is
speculative. The calibration harness operationalizes the Python predecessor's
calibration issue (#585) into a repeatable, automated quality gate. Cross-reference
#585 (original calibration issue), #1421 (feedback-loop signals as corpus source).

**Forward reference:** #1422. Depends on positive-signal corpus curation (GitHub
org Project #19). See also #553 (`eval` CLI subcommand, which the harness would
exercise).

---

### 9.10 Prior-PR / change-history context source — #1423 (P3, MAY)

**Requirement:**

The context orchestrator (`integrations/context/`) MAY add a `prior_pr_context`
source that, given a PR's `(owner, repo, author)`, fetches the last
`TRUSTY_REVIEW_PRIOR_PR_COUNT` (default `3`) merged PRs by the same author from
the GitHub API and includes a compact summary of their verdicts, any recurring
findings, and the trend (improving/stable/declining). This context is passed to
the reviewer prompt as a named block.

This is complementary to, but distinct from, the longitudinal contributor profile
(`profile/` pipeline, already ✅): the profile is a periodic batch-synthesized
narrative; the prior-PR context source is a lightweight, per-review, recent-history
snapshot fetched just-in-time.

**Rationale:** A reviewer who knows "this author's last 3 PRs all had the same
missing-error-check pattern" can weigh findings more appropriately and tailor
the tone. This is standard practice in Duetto's highest-rated human reviews.
The MAY priority reflects that the longitudinal profile (already ✅) partially
satisfies this need; this source adds recency and is cheaper to compute.

**Forward reference:** #1423. Depends on GitHub PR history API access (already
available via `integrations/github/`). Low implementation risk; gated P3 because
profile pipeline (#558) already covers the deeper version.
