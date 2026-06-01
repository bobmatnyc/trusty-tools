# 07 — Data Models & Persistence

**Status:** DRAFT
**Part of:** [trusty-review spec](README.md)
**Cross-refs:** [02-pipeline](02-pr-review-pipeline.md) · [03-diff-summarizer](03-diff-summarizer.md) · [05-integrations](05-integrations.md) · [08-interfaces](08-interfaces.md)
**Factual basis:** source-analysis §5 (PRReviewResult, FixSuggestion, dedup), §3.3 (diff models), §13 (storage delta).

All models SHALL be `serde`-(de)serializable so the JSON log schema (§5) is the canonical wire shape. Enums use lowercase string serialization unless noted.

---

## 1. ReviewResult

**REV-600 — ReviewResult fields.** Direct port of the Python `PRReviewResult` field set (source-analysis §5.1). Field names SHALL be preserved on the JSON wire so existing log tooling keeps working.

| Field | Type | Notes |
|-------|------|-------|
| `owner`, `repo` | String | GitHub org/repo |
| `pr_number` | u64 | |
| `pr_title`, `pr_url` | String | |
| `review_body` | String | Full LLM markdown output |
| `analysis` | Object | `grade`, `display_grade`, `display_grade_is_fallback`, `hotspots_in_pr`, `smells_in_pr`, `copilot_mode`, `bot_review_count` |
| `context_files` | Vec\<String\> | Filenames from vector-search context |
| `input_tokens`, `output_tokens` | u32 | Final reviewer call only |
| `cost_estimate_usd` | f64 | |
| `dry_run`, `posted` | bool | |
| `timestamp` | String | ISO-8601 |
| `error` | Option\<String\> | Non-None on pipeline error |
| `fix_suggestions` | Vec\<FixSuggestion\> | §2 |
| `filed_issue_numbers` | Vec\<u64\> | GitHub issues filed |
| `review_mode` | String | `full` \| `no_context` |
| `head_sha` | String | dedup key; empty on pre-field records |
| `tracker_issue_number` | Option\<u64\> | |
| `tracker_issue_url` | Option\<String\> | |
| `diff` | Option\<String\> | Stored diff for eval replay |
| `apex_context` | Vec\<Object\> | slim metadata (bodies stripped) |
| `jira_context` | Vec\<Object\> | slim |
| `confluence_context` | Vec\<Object\> | slim |
| `github_issues_context` | Vec\<Object\> | slim |
| `cross_ref_context` | Vec\<Object\> | slim (blast-radius hits) |
| `copilot_mode` | String | `full` \| `supplement` \| `duetto_only` |
| `bot_review_count` | u32 | |
| `multipass` | bool | |
| `pass1_tokens`, `pass2_tokens` | u32 | multi-pass cost breakdown |
| `trigger` | String | `manual` \| `auto` |
| `grade` | String | top-level mirror of `analysis.grade` |
| `enrichment_rounds` | u32 | |
| `enrichment_queries` | Vec\<String\> | |
| `skip_reason` | Option\<String\> | short token on pipeline skip |
| `review_version` | String | pipeline version (e.g. `tr-0.1`; doc 02 REV-118) |

**REV-601 — Context slimming.** The `*_context` vectors SHALL store **slim** metadata (paths, line refs, short excerpts) with full bodies stripped, mirroring Python `_slim_context()`. This bounds log size. (source-analysis §5.1)

---

## 2. FixSuggestion

**REV-602 — FixSuggestion fields.** (source-analysis §5.2)

| Field | Type | Notes |
|-------|------|-------|
| `file` | String | Changed file path |
| `line` | Option\<u32\> | Optional line number |
| `kind` | String | Finding type |
| `description` | String | Human-readable finding |
| `suggestion` | String | Proposed fix |
| `confidence` | f32 | 0.0–1.0 |
| `effort` | String (enum) | `low` \| `medium` \| `high` |

**REV-603 — Issue eligibility derived fields (transient).** During the pipeline, each finding carries transient flags not necessarily persisted: `verified` (CONFIRMED/REFUTED/skipped), `issue_eligible` (confidence ≥ `issue_threshold` AND effort ∈ `("low","medium")`). These drive Stage 13–15 (doc 02). (source-analysis §2.2, §2.3)

---

## 3. Verdict & confidence enums

**REV-604 — Verdict enum.** `Verdict ∈ { Approve, ApproveStar, RequestChanges, Block, NotApplicable }` serialized as `APPROVE` / `APPROVE*` / `REQUEST_CHANGES` / `BLOCK` / `N/A`. (source-analysis §2.1; doc 02 §2.1)

**REV-605 — Effort enum.** `Effort ∈ { Low, Medium, High }`. Only `Low`/`Medium` are issue-eligible (`FIX_ISSUE_ALLOWED_EFFORTS`). (source-analysis §2.3)

**REV-606 — Verification outcome enum.** `VerifyOutcome ∈ { Confirmed, Refuted, ErrorRefuted, TruncationRefuted, Skipped }` for telemetry. `ErrorRefuted` carries the `LlmError` class so config/lifecycle failures are distinguishable in logs (doc 04 REV-340). (source-analysis §2.2, §12.1)

**REV-607 — Confidence is `f32` in `[0.0, 1.0]`.** Constructors SHALL clamp out-of-range values rather than error (defensive against malformed LLM JSON).

---

## 4. Diff-summarizer models

**REV-608** — As specified in [doc 03 §4](03-diff-summarizer.md) (source-analysis §3.3): `FilteredDiff`, `FilteredFile`, `FilteredHunk`, `DroppedHunk`, `DroppedFile`, `FileDisposition` (`KEPT`/`DROPPED`/`SUMMARY_ONLY`), `HunkDropReason` (`WHITESPACE_ONLY`/`IMPORT_ONLY`/`COMMENT_ONLY`/`MECHANICAL_HAIKU`). These live in `src/diff/models.rs`.

---

## 5. Dry-run JSON log schema

**REV-610 — On-disk layout.** `{PR_INTELLIGENCE_LOG_DIR}/{owner}/{repo}/{pr_number}/review.{json,md}` (source-analysis §5.3):
- `review.json` — serialized `ReviewResult` (§1). This IS the canonical schema.
- `review.md` — the human-readable `review_body` plus a header (verdict, grade, links).

**REV-611 — Schema stability.** The JSON SHALL be additively versioned via `review_version`. New fields are added with serde defaults; existing field names are not renamed (back-compat with Python logs where feasible). (source-analysis §5.1)

---

## 6. Persistence: dedup store & review log

### 6.1 Persistence choice — RECOMMENDATION

**REV-620 — Use redb for the cross-process dedup claim.** The Python service used SQLite (`dedup.db`); trusty-review SHALL use **redb** for the dedup claim store.

| Option | Pros | Cons | Verdict |
|--------|------|------|---------|
| **redb (RECOMMENDED)** | Already the workspace standard (`redb` is a workspace dep; trusty-analyze/trusty-search use it; `trusty-analyze.facts.redb` exists). Pure-Rust, embedded, ACID, no external process. Aligns deps. | Need an atomic insert-or-fail pattern over a key table. | **CHOSEN** |
| SQLite (`rusqlite`) | Direct parity with Python; familiar `INSERT OR FAIL`. | Adds a non-standard dep (sqlite native lib) the workspace otherwise avoids. | Rejected for dep hygiene. |

  > **Rationale (source-analysis §13 storage delta):** the delta table explicitly leaves dedup storage "redb … or SQLite; TBD in spec." We choose redb to match the workspace and avoid a native SQLite dependency.

**REV-621 — Dedup claim semantics.** A redb table keyed on `(owner, repo, pr_number, head_sha)` SHALL implement an atomic **claim** (write within a transaction that fails if the key exists and is not stale) and a **release** (delete on completion). Stale claims older than `DEDUP_STALE_SECS` (7200) SHALL be purged on each claim attempt. Release SHALL run in a guard (`Drop` or explicit `finally`-equivalent) to prevent leaked claims. (source-analysis §5.3, §12.4; doc 02 REV-101c)

### 6.2 Review log backend

**REV-622 — Filesystem JSON/MD by default.** The review log SHALL default to the filesystem layout (REV-610), matching today's schema. The store module SHALL expose a `ReviewLog` trait so an alternative backend (e.g. redb or object storage) can be plugged later without pipeline changes. (source-analysis §5.3, §13)

---

## 7. Calibration tracker (upsert-per-PR)

**REV-630 — Calibration record.** In dry-run, a calibration record SHALL be upserted **per PR** (not per push): keyed by `(owner, repo, pr_number)`, updated in place on re-review. It mirrors the tracker-issue upsert discipline. (source-analysis §4.3, §12.7)
  > **Rationale (lesson learned §12.7):** per-push records accumulated duplicate calibration issues; upsert-per-PR fixes it.

**REV-631 — Calibration fields.** `owner`, `repo`, `pr_number`, `trigger`, `grade`/`verdict`, `pr_url`, `review_excerpt`, `head_sha`, `timestamp`, `calibration_issue_number`. Persisted alongside (or within) the review log. (source-analysis §4.3)
