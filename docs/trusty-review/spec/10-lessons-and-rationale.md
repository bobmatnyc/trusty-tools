# 10 — Lessons & Rationale (Binding Requirements Ledger)

**Status:** DRAFT
**Part of:** [trusty-review spec](README.md)
**Cross-refs:** every other spec doc.
**Factual basis:** source-analysis §12 (13 hard-won lessons), §13 (NEW-vs-today delta).

This document distills the 13 hard-won lessons from the Python production system into binding, testable requirements, each as **symptom → fix → requirement**. It is the cross-cutting requirements ledger that the other docs trace back to. The NEW-vs-today delta table closes the document.

---

## 1. Lessons → binding requirements

### L1 — Inactive verifier model → silent all-approve (source-analysis §12.1)

- **Symptom:** Every blocking finding auto-refuted; every PR effectively APPROVE regardless of content.
- **Root cause:** `_VERIFICATION_MODEL_ID` hardcoded to a `LEGACY`-lifecycle Haiku ID; every verification call threw `ResourceNotFoundException`; the handler set `confirmed = false` → REFUTED for all → downgrade to APPROVE.
- **Fix (today):** Source the verifier model from the ACTIVE-lifecycle constant; classify the error at ERROR level as `pr_review_verification_model_error`.
- **Binding requirement:** **REV-340** (provider classifies config/lifecycle errors distinctly + emits `verification_model_error` + alarms), **REV-341 / REV-811** (startup model probe; refuse to start live if verifier inactive), **REV-810** (alarm), **REV-813** (verdict-distribution monitoring as a second signal), **REV-334** (do not retry config/lifecycle errors).

### L2 — Bedrock inference-profile prefix (source-analysis §12.2)

- **Symptom:** Bedrock calls fail with `ValidationException`/`ResourceNotFoundException` on bare model IDs.
- **Root cause:** Cross-region inference profiles require the `us.` prefix.
- **Fix (today):** All production model IDs use `us.`.
- **Binding requirement:** **REV-320** (Bedrock provider requires/validates `us.` prefix), **REV-552** (config-time validation, fail loudly at startup), **REV-322** (OpenRouter is exempt — uses slugs).

### L3 — Analyzer readiness false `corpus_not_ready` (source-analysis §12.3)

- **Symptom:** Every review logged trusty-analyze unavailable even when it was healthy.
- **Root cause:** `has_analysis()` probed the O(corpus) `/quality` endpoint (11–103s) with a 5s timeout → always timed out.
- **Fix (today):** Two cheap checks — `GET /health` + `GET /indexes`; never `/quality`.
- **Binding requirement:** **REV-441 / REV-013 / REV-805.2** (two-step cheap probe; never probe O(corpus); check `search_reachable`).

### L4 — Concurrent webhook delivery → duplicate reviews (source-analysis §12.4)

- **Symptom:** Multiple webhook events per push ran the pipeline twice → duplicate calibration issues.
- **Fix (today):** Three-layer dedup (webhook event filter + in-process guard + cross-process SQLite claim).
- **Binding requirement:** **REV-702** (only `review_requested` dispatches), **REV-101** (three dedup layers), **REV-621** (redb atomic claim + stale purge + release guard), **REV-705** (claim pre-SHA guard before spawn).

### L5 — Verifier candidate selection bug (sub-0.90 never verified) (source-analysis §12.5)

- **Symptom:** Real defects at confidence 0.65–0.82 never reached the verifier; narrative said REQUEST_CHANGES but findings sat below the 0.90 verify gate → downgrade to APPROVE\*.
- **Root cause:** Candidate selection used only `confidence ≥ block_threshold` (0.90) regardless of verdict.
- **Fix (2026-05-30):** When verdict ∈ {REQUEST_CHANGES, BLOCK}, verify ALL findings with `confidence ≥ 0.50`, sorted desc, capped at 5.
- **Binding requirement:** **REV-135** (verdict-conditioned candidate selection), **REV-134** (grade-divergence resolved by verification), **REV-142** (`VERIFY_CANDIDATE_MIN_CONFIDENCE = 0.50`).

### L6 — Verifier truncation shortcut over-eagerness (source-analysis §12.6)

- **Symptom:** Real findings at char 4001–16384 auto-refuted without an LLM call.
- **Root cause:** The truncation shortcut checked a 4k-char verification slice, not the full diff.
- **Fix (today):** `diff_for_verify` is the full `diff_text` (bounded to `MAX_DIFF_CHARS`); auto-refute only when BOTH file path AND line marker are absent from the FULL diff.
- **Binding requirement:** **REV-138** (full-diff verification window), **REV-136** (truncation-shortcut precondition).

### L7 — Calibration tracker upsert-per-PR (source-analysis §12.7)

- **Symptom:** Each re-push created a new tracker issue (8+ duplicates per PR).
- **Fix (today):** Search existing `code-review`-labeled issue by title prefix; update in place; create only if absent.
- **Binding requirement:** **REV-405 / REV-406** (one tracker issue per PR, upsert), **REV-630** (calibration record upsert-per-PR).

### L8 — Dry-run rollout discipline (source-analysis §12.8)

- **Symptom (risk):** Premature switch to live before quality validated.
- **Fix (today):** Dry-run default ON; pipeline runs fully but posts nothing; only `review_requested` dispatches.
- **Binding requirement:** **REV-540** (dry-run default ON), **REV-820** (staged rollout), **REV-702** (event gate independent of dry-run), **REV-117** (dry vs live output paths).

### L9 — Suppression fail-open (source-analysis §12.9)

- **Symptom (design principle):** A GitHub API error during suppression must never block a review.
- **Fix (today):** Every suppression/config lookup wraps errors and returns empty.
- **Binding requirement:** **REV-115 / REV-405S / REV-511 / REV-532 / REV-014** (all suppression + config lookups fail-open).

### L10 — Graceful degradation when trusty-analyze is down (source-analysis §12.10)

- **Symptom (design):** Optional sidecar down should not block the review.
- **Fix (today):** `has_analysis()` returns false → empty analysis; `PRReviewServiceUnavailable` raised only for trusty-search + LLM.
- **Binding requirement:** **REV-011 / REV-012 / REV-442** (required vs optional dependency distinction; analyze degrades silently), **REV-452** (failure notice only for required deps).

### L11 — Push firewall (source-analysis §12.11)

- **Symptom:** Bot caught force-pushing to an infra repo.
- **Fix (today):** `GH_ALLOW_PUSH = False` hard-coded; `_assert_no_push_operation()` raises on any write.
- **Binding requirement:** **REV-403** (hard-coded, non-configurable push firewall), **REV-721** (applies to all CLI paths), **NG1** (read + comment only).

### L12 — Noisy fixture/generated file diff (source-analysis §12.12)

- **Symptom:** i18n/fixture churn buried real changes; bot burned context budget on noise (PR #9545).
- **Fix (today):** `NOISY_FILE_PATTERNS` 1-line collapse + DiffAnalyzer Stage A/B/C drops.
- **Binding requirement:** **REV-211** (noisy-file collapse), **REV-201–REV-208** (Stage A/B/C), **REV-102** (truncation + summarizer in diff fetch).

### L13 — Cross-reference blast-radius gap (source-analysis §12.13)

- **Symptom:** PR #9545 deleted a permission check; the orphaned producer lived in an untouched file; diff-only similarity missed it.
- **Fix (today):** `_search_cross_references()` extracts enum constants / method calls from deleted lines, searches unchanged files, prioritizes producer keywords.
- **Binding requirement:** **REV-108** (cross-reference blast-radius search in parallel context retrieval).

---

## 2. Lesson → requirement traceability matrix

| Lesson | Primary requirements | Doc(s) |
|--------|----------------------|--------|
| L1 inactive verifier | REV-340, REV-341, REV-811, REV-810, REV-813, REV-334 | 04, 09 |
| L2 Bedrock prefix | REV-320, REV-552, REV-322 | 04, 06 |
| L3 analyzer probe | REV-441, REV-013, REV-805 | 01, 05, 09 |
| L4 concurrent webhooks | REV-702, REV-101, REV-621, REV-705 | 02, 07, 08 |
| L5 candidate selection | REV-135, REV-134, REV-142 | 02 |
| L6 truncation window | REV-138, REV-136 | 02 |
| L7 tracker upsert | REV-405, REV-406, REV-630 | 05, 07 |
| L8 dry-run discipline | REV-540, REV-820, REV-702, REV-117 | 02, 06, 08, 09 |
| L9 suppression fail-open | REV-115, REV-511, REV-532, REV-014 | 02, 06, 01 |
| L10 graceful degradation | REV-011, REV-012, REV-442, REV-452 | 01, 05 |
| L11 push firewall | REV-403, REV-721, NG1 | 05, 08, README |
| L12 noisy diffs | REV-211, REV-201–208, REV-102 | 02, 03 |
| L13 blast radius | REV-108 | 02 |

---

## 3. NEW vs Today — delta table

Reproduced from source-analysis §13, annotated with the spec requirements that realize each delta.

| Dimension | Today (Python) | NEW (trusty-review Rust) | Realized by |
|-----------|----------------|--------------------------|-------------|
| Language | Python 3.11+, FastAPI | Rust, axum 0.8 | REV-001..006, REV-700 |
| LLM provider | AWS Bedrock only (Converse) | **Co-equal** Bedrock + OpenRouter behind a trait | REV-300, REV-301 |
| Model selection | Fixed by env var | Selectable **per run AND per role** (reviewer/verifier/summarizer) | REV-310, REV-311, REV-312 |
| JIRA context | ATLASSIAN_* REST + vector fallback | Same logic; configurable JIRA REST client | REV-410, REV-411 |
| APEX context | Separate VectorSearchAdapter instance | **APEX as a repo** in trusty-search; same index query | REV-420, REV-421 |
| Deployment | FastAPI app embedded in code-intelligence | **Standalone** Rust service; server OR CLI, one binary | REV-010, REV-800 |
| Dedup storage | SQLite (`dedup.db`) | **redb** (workspace standard) | REV-620, REV-621 |
| Log storage | Filesystem JSON/MD | Filesystem JSON/MD (same schema); pluggable `ReviewLog` trait | REV-610, REV-622 |
| GitHub auth | PyGithub + httpx + GithubIntegration | reqwest/octocrab + App JWT in Rust (`jsonwebtoken`) | REV-400, REV-401 |
| Suppression matching | Python regex + word overlap | Same algorithm in Rust | REV-530 |
| Diff analyzer | Python + Haiku LLM | Rust deterministic Stages A/B + LLM Stage C via provider trait | REV-200..208 |
| Webhook server | FastAPI route | axum route (trusty-analyze/search pattern) | REV-700, REV-701 |
| Multi-org | INSTALLATION_ID_* per org | Same; config-driven | REV-402 |

---

## 4. Acceptance checklist (spec → implementation gate)

An implementation is spec-conformant only when ALL of the following hold:

- [ ] Verifier-model misconfig produces a `verification_model_error` alarm and (live) refuses to start — never a silent all-approve. (L1)
- [ ] Bedrock model IDs validated for `us.` prefix at config time. (L2)
- [ ] trusty-analyze readiness uses `/health`+`/indexes` only. (L3)
- [ ] Three dedup layers present; redb claim atomic + released. (L4)
- [ ] Verification candidate selection is verdict-conditioned (≥0.50 when REQUEST_CHANGES/BLOCK). (L5)
- [ ] Verification uses the full diff window. (L6)
- [ ] One tracker issue per PR, upserted. (L7)
- [ ] Dry-run default ON; only `review_requested` dispatches. (L8)
- [ ] All suppression/config lookups fail-open. (L9)
- [ ] trusty-search + LLM required; trusty-analyze optional/degrades. (L10)
- [ ] Push firewall hard-coded, non-configurable. (L11)
- [ ] Noisy fixtures collapsed; Stage A/B/C diff filtering present. (L12)
- [ ] Cross-reference blast-radius search present. (L13)
- [ ] LLM provider trait with co-equal Bedrock + OpenRouter, per-run/per-role model selection. (binding decision #2)
- [ ] APEX treated as a repo in the primary index. (binding decision #3)
- [ ] Runs as both CLI one-shot and webhook server from one binary. (binding decision #4)
