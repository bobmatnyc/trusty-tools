# 05 — Integrations

**Status:** DRAFT
**Part of:** [trusty-review spec](README.md)
**Cross-refs:** [01-architecture](01-architecture.md) · [02-pipeline](02-pr-review-pipeline.md) · [06-configuration](06-configuration.md) · [08-interfaces](08-interfaces.md) · [10-lessons](10-lessons-and-rationale.md)
**Factual basis:** source-analysis §4 (GitHub), §6 (adapters), §7 (JIRA/APEX), §9 (Slack), §11.5 (HTTP APIs consumed), §12 (lessons).
**Workspace grounding:** `crates/trusty-analyze/src/core/github.rs` already implements `fetch_pr_diff`, `post_pr_comment`, `verify_webhook_signature`, typed `GithubError` (verified) — reuse where possible.

---

## 1. GitHub integration

### 1.1 Authentication

**REV-400 — GitHub App preferred, PAT fallback.** trusty-review SHALL prefer GitHub App authentication and fall back to a PAT. (source-analysis §4.1)

| Input | Purpose |
|-------|---------|
| `GITHUB_APP_ID`, `GITHUB_APP_CLIENT_ID` | App identity |
| `GITHUB_APP_PRIVATE_KEY` (RSA PEM, `\n`-escaped) | Signs the App JWT |
| `GITHUB_INSTALLATION_ID_DUETTORESEARCH` | duettoresearch org installation |
| `GITHUB_INSTALLATION_ID_HOTSTATS` | hotstats org installation |
| `GITHUB_TOKEN` | PAT fallback |

**REV-401 — App JWT in Rust.** The crate SHALL mint the GitHub App JWT in Rust (`jsonwebtoken` is already a workspace dependency) and exchange it for a short-lived installation token. (source-analysis §13 "GitHub auth")

**REV-402 — Multi-org token selection.** A `resolve_token(owner)` SHALL select the installation token by case-insensitive `owner` match (duettoresearch vs hotstats), falling back to PAT when App auth is not configured. (source-analysis §4.1)

### 1.2 Push firewall (BINDING)

**REV-403 — Read + comment only; hard-coded push firewall.** A `GH_ALLOW_PUSH = false` constant SHALL be hard-coded and NOT configurable by env or per-repo config. Any attempt to invoke a git-write operation (branch create, file commit, PR create, force-push) SHALL panic/error via an `assert_no_push_operation()` guard.
  > **Rationale (lesson learned §12.11):** the bot was caught force-pushing to an infra repo. The firewall is intentionally non-configurable. (binding non-goal NG1)

### 1.3 Permitted GitHub operations

**REV-404** — The bot is restricted to (source-analysis §4.2):
- POST PR review comment.
- GET PR diff, file list, metadata.
- GET/POST/PATCH issues (tracker + calibration).
- GET/POST labels; POST issue type (REST numeric `TASK_ISSUE_TYPE_NUMERIC_ID = 203410`).
- POST GraphQL for GitHub Project v2 add.
- GET `.github/code-intelligence.yml`.
- GET existing PR reviews (Copilot/bot detection).

### 1.4 Tracker-issue-per-PR semantics

**REV-405 — One tracker issue per PR (upsert).** A single GitHub issue per PR SHALL be created on first review and **updated in place** on every re-review. (source-analysis §4.3, §12.7)

| Property | Value |
|----------|-------|
| Labels | `["code-review", "code-intelligence"]` |
| Issue type | "Task" (GraphQL node `IT_kwDOABbzg84AAxqS`) |
| Project discovery label | `code-intelligence` (for Project #16 filter) |
| Title pattern | `[PR Review] {owner}/{repo}#{pr_number} — {VERDICT}` |
| Suppression prefix length | first 60 chars after the marker (`SUPPRESS_PATTERN_PREFIX_LEN`) |

**REV-406 — Upsert algorithm.** Search existing `code-review`-labeled issues for a matching title prefix; if found, PATCH body + title; else create. (source-analysis §4.3)
  > **Rationale (lesson learned §12.7):** without upsert, each re-push created a new tracker issue (8+ duplicates per active PR).

**REV-407 — Calibration issues (dry-run).** In dry-run, file a calibration issue with label `code-intelligence-review`, added to a `PR Review Calibration` GitHub Project (created if missing); body includes trigger, grade, PR link, full review text. (source-analysis §4.3)

**REV-408 — Fix-suggestion issues.** When live and a finding is issue-eligible (confidence ≥ `issue_threshold`, effort ∈ `("low","medium")`, cap `FIX_ISSUE_MAX_PER_PR` = 5), the finding contributes to the tracker issue body. v0.1 SHALL NOT open separate auto-fix PRs (NG5; firewall REV-403). (source-analysis §2.3, §4.3)

### 1.5 Dismissal / suppression control flow (developer-facing)

**REV-409 — Tracker control semantics.** Re-specify the developer controls (CLAUDE.md "Handling Incorrect Bot Findings"):
- Close the tracker issue → bot does not re-open or re-file for the same PR.
- Close + `bot-suppress` label → extract a pattern from the issue title and suppress similar findings in all future PRs for that repo.
- `wontfix` label → same effect as closing for that PR.

---

## 2. Suppression integration (GitHub side)

**REV-405S → see REV-115/REV-530.** Two suppression sources, merged at review time, both **fail-open** (source-analysis §4.4, §8.2, §12.9):
1. `fetch_suppressed_patterns()` — repo issues with `bot-suppress` label + closed state → extract pattern from title.
2. `fetch_repo_suppress_config()` — `suppress` list from `.github/code-intelligence.yml`.

Matching: case-insensitive substring OR Jaccard word overlap ≥ 0.70 (`SUPPRESS_OVERLAP_THRESHOLD`). (doc 06 REV-530)

---

## 3. JIRA / ticket context

**REV-410 — Ticket-ID extraction.** Extract ticket IDs matching `\b([A-Z][A-Z0-9]+-\d+)\b` from PR title + description. (source-analysis §7.1)

**REV-411 — Fallback chain.** (source-analysis §7.1)
1. If `ATLASSIAN_EMAIL` + `ATLASSIAN_API_TOKEN` + `ATLASSIAN_URL` configured AND ticket IDs found → `GET /rest/api/3/issue/{id}` per ticket (up to `MAX_KNOWLEDGE_RESULTS` = 3).
2. If REST returns nothing → per-ticket keyword vector search against the knowledge index.
3. If no credentials → vector search fallback.
4. Final fallback → semantic search on PR title + description.

**REV-412 — Fail-open.** Any JIRA error → empty ticket context; never blocks the review. (source-analysis §12.9)

---

## 4. APEX-as-repo (BINDING decision #3)

**REV-420 — APEX is a repo, not a separate index.** trusty-review SHALL treat APEX product specs as a repository **indexed alongside code in trusty-search** and query it through the **same** index query used for code context — NOT via a bespoke separate adapter instance.
  > **Rationale (source-analysis §7.2, §13):** today APEX is a separate `apex_search` VectorSearchAdapter. The NEW design folds APEX into the primary index so a single search returns apex spec chunks alongside code chunks.

**REV-421 — Implementation options (spec both; recommend).**
- **(A, RECOMMENDED)** Single primary index containing code + APEX repo; one `POST /indexes/{index}/search` returns both. trusty-review filters/labels APEX chunks by path prefix for citation.
- **(B, fallback)** A configurable secondary index URL/name (`TRUSTY_SEARCH_APEX_INDEX`) queried with the same cross-query, for deployments that keep APEX in its own index. The pipeline merges results.

**REV-422 — Citation format.** APEX hits SHALL be cited in the review as ``[apex: `path/to/spec.md:15` — "brief excerpt"]``, capped at `MAX_APEX_RESULTS` = 2. (source-analysis §7.2)

---

## 5. trusty-search client (`:7878`) — REQUIRED dependency

**REV-430 — Client contract.** A `SearchClient` (HTTP default, doc 01 REV-009) over `TRUSTY_SEARCH_URL` (default `http://localhost:7878`), index from `TRUSTY_SEARCH_INDEX` (default `main`). (source-analysis §6.1, §11.5)

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/health` | GET | Liveness; `embedder` status, `search_reachable` |
| `/indexes` | GET | List registered indexes |
| `/indexes/{id}/status` | GET | Chunk count + root path |
| `/indexes/{id}/search` | POST | Hybrid BM25 + vector search |

**REV-431 — REQUIRED dependency.** If trusty-search is unreachable, the review SHALL be **skipped** (not approved) and a Slack service-failure notice sent (REV-450). (source-analysis §9, §12.10; doc 01 REV-011)

---

## 6. trusty-analyze client (`:7879`) — OPTIONAL dependency

**REV-440 — Client contract.** An `AnalyzeClient` over `PR_INTELLIGENCE_ANALYZER_URL` (default `http://localhost:7879`), index from `TRUSTY_SEARCH_INDEX`. (source-analysis §6.2, §11.5)

| Method | Endpoint | Timeout | Notes |
|--------|----------|---------|-------|
| `health()` | `GET /health` | 5s | liveness + `search_reachable` |
| `is_available()` | `GET /health` | 5s | cache 60s TTL |
| `has_analysis()` | `GET /health` **+** `GET /indexes` | 5s each | **cheap two-step readiness probe** |
| `complexity_hotspots()` | `GET /indexes/{id}/complexity_hotspots` | 180s | hotspot list |
| `smells()` | `GET /indexes/{id}/smells` | 180s | smell list |
| `quality()` | `GET /indexes/{id}/quality` | 180s | O(corpus) — **NEVER a readiness probe** |

**REV-441 — Two-step readiness probe (BINDING).** `has_analysis()` SHALL use ONLY `GET /health` (O(1)) + `GET /indexes` (O(n_indexes)). It SHALL NEVER call `/quality`.
  > **Rationale (lesson learned §12.3):** probing the O(corpus) `/quality` endpoint with a 5s timeout always timed out (it streams ~396k chunks, 11–103s), making the sidecar look perpetually unavailable. The `/health` response carries a `search_reachable` boolean — check it explicitly.

**REV-442 — Graceful degradation.** All `AnalyzeClient` methods SHALL catch connect/timeout/HTTP errors and return an empty default. trusty-analyze failure SHALL NOT raise the service-unavailable path; the review proceeds with empty static-analysis context. (source-analysis §6.2, §12.10; doc 01 REV-012)

---

## 7. Slack notifications

**REV-450 — Two notification types** (source-analysis §9):
- `notify_pr_review_complete()` — single-line mrkdwn: dry-run/live label, linked `owner/repo#PR`, verdict, findings count, optional calibration/tracker issue link. **Never raises** (a Slack outage cannot break a review).
- `notify_pr_review_service_failure()` — Block Kit warning when a **required** service (trusty-search or LLM) is unavailable; includes service name, PR URL, truncated error (≤500 chars).

**REV-451 — Dispatch precedence.** Prefer `SLACK_NOTIFICATION_CHANNEL` + `SLACK_BOT_TOKEN` (`chat.postMessage`); fall back to `SLACK_NOTIFICATION_WEBHOOK`. (source-analysis §9)

**REV-452 — Failure trigger.** The service-failure notice fires on the "required service unavailable" condition (trusty-search / LLM), mirroring the Python `PRReviewServiceUnavailable` path; trusty-analyze unavailability does NOT trigger it (REV-442). (source-analysis §9, §12.10)
