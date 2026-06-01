# trusty-review Source Analysis

**Purpose**: Grounded analysis of the existing Python PR auto-review system for use by a spec author writing the trusty-review Rust crate specification.

**Sources analyzed**:
- `/Volumes/SSD1/Duetto/repos/code-intelligence/src/code_intelligence/services/pr_review_service.py` (6,990 lines, REVIEW_VERSION = "v1.4")
- `diff_analyzer/` subpackage (analyzer.py, file_filter.py, hunk_classifier.py, hunk_filter.py, models.py)
- `pr_review_github.py`, `pr_review_log.py`, `slack_notify.py`
- `adapters/analyzer_adapter.py`, `adapters/vector_search.py`, `adapters/bedrock_llm.py`
- `web/routes/pr_review.py`, `cli/pr_review.py`
- `docs/repo-config.md`, repo root `CLAUDE.md`
- `/Users/masa/Projects/trusty-tools/` — Cargo workspace (16 crates), `CLAUDE.md`, trusty-analyze and trusty-search crate layouts

---

## 1. End-to-End Review Pipeline

### 1.1 Entry Points

| Entry | Location | Key parameters |
|---|---|---|
| GitHub webhook | `web/routes/pr_review.py` `POST /pr/github/webhook` | HMAC-verified, fires `review_pr` as `asyncio.create_task` |
| CLI manual run | `cli/pr_review.py` `pr-review run` | `--no-context` flag, `--multipass` flag |
| CLI eval | `cli/pr_review.py` `pr-review eval` | `skip_dedup=True`, multi-model |
| CLI compare | `cli/pr_review.py` `pr-review compare` | full vs no-context side-by-side |

### 1.2 Webhook Event Filtering (`web/routes/pr_review.py`)

Only `review_requested` triggers a review. All other `pull_request` actions (`opened`, `synchronize`, `reopened`) are accepted with 200 OK but **not dispatched**. `closed` is also short-circuited with no dispatch.

**Trigger classification** (determined entirely from request context, not env vars):
- `requested_reviewer.login == PR_REVIEW_BOT_USERNAME` → `trigger="manual"`, `force_live=True` (posts live regardless of `PR_INTELLIGENCE_DRY_RUN`)
- `requested_reviewer.login` in `live_review_requesters` set → `trigger="manual"`, `force_live=True`
- Any other `review_requested` → `trigger="auto"`, `force_dry_run=True` (forced dry-run, files calibration issue)

`effective_dry_run = (settings.pr_intelligence.dry_run OR force_dry_run) AND NOT force_live`

**Author exclusion**: PRs from logins in `PR_INTELLIGENCE_EXCLUDED_AUTHORS` are silently skipped before dispatch.

HMAC verification uses `X-Hub-Signature-256` header, `hmac.compare_digest` for constant-time comparison.

### 1.3 Pipeline Stage Sequence (`PRReviewService.review_pr` → `_review_pr_inner` → `_run_review_pipeline` → `_run_review_pipeline_inner`)

```
1. Eligibility check
   ├── _is_repo_enabled(): checks PR_INTELLIGENCE_ENABLED_REPOS / EXCLUDED_REPOS
   └── Returns early if not enabled

2. Deduplication (two layers)
   ├── _PR_IN_FLIGHT set: per-(owner,repo,pr_number) in-process guard (pre-SHA)
   ├── _IN_FLIGHT set: per-(owner,repo,pr_number,head_sha) in-process guard
   └── PRReviewLog.claim_review(): SQLite INSERT on composite PK (cross-process)

3. Diff fetch (_fetch_pr_diff)
   ├── PyGithub: PR metadata, file list, unified diff
   ├── _summarize_noisy_diff_files(): collapse large fixture/i18n diffs to 1-line
   └── _build_diff_text(): truncate to MAX_DIFF_CHARS (16,384 chars ≈ 4k tokens)
       Uses DiffAnalyzer if available; falls back to raw diff

4. Repo config fetch (_fetch_repo_config)
   ├── GET .github/code-intelligence.yml via GitHub Contents API
   ├── Falls back to DEFAULT_REPO_CONFIG on any error (fail-open)
   └── If skip:true → return None immediately

5. Existing bot-review detection (_fetch_existing_bot_reviews)
   ├── Fetch prior Copilot/bot PR reviews
   └── _determine_copilot_mode() → "full" | "supplement" | "duetto_only"

6. Head-SHA dedup
   ├── PRReviewLog.show() to check existing review for this PR
   └── If existing.head_sha == current head_sha AND NOT skip_dedup → return cached

7. Parallel context retrieval
   ├── _search_context(): trusty-search POST /indexes/main/search (vector search)
   │   Query = PR title + changed file paths
   │   Up to MAX_CONTEXT_FILES=6 chunks
   │   If require_search_context=True and zero results → skip review entirely
   ├── _fetch_jira_context(): JIRA tickets (REST API or vector fallback)
   ├── _fetch_confluence_context(): Confluence pages (vector search)
   ├── apex search: product specs from apex_search VectorSearchAdapter
   ├── github_search: GitHub issues context
   └── _search_cross_references(): cross-repo blast-radius search

8. Iterative enrichment (_run_enrichment_rounds, up to MAX_ENRICHMENT_ROUNDS=5)
   ├── Extracts identifiers from diff (class/method/const names)
   └── Runs targeted vector searches per identifier (if new chunks found)

9. Static analysis (_analyze_changed_files)
   ├── AnalyzerAdapter.has_analysis(): cheap readiness probe (GET /health + GET /indexes)
   ├── AnalyzerAdapter.complexity_hotspots(): GET /indexes/{id}/hotspots
   ├── AnalyzerAdapter.smells(): GET /indexes/{id}/smells
   └── Graceful degradation if unavailable

10. LLM review generation (_generate_review)
    ├── _build_user_message(): assembles all context into structured markdown prompt
    ├── BedrockClient.chat() → Claude Sonnet (us.anthropic.claude-sonnet-4-6)
    └── Returns (review_body, input_tokens, output_tokens)

11. Grade extraction (_extract_review_grade)
    └── Regex scan of last 20% of review_body against _GRADE_PATTERNS

12. Fix-suggestion parsing (_parse_fix_suggestions)
    └── Parses structured ```fix_suggestions JSON blocks from review body

13. Verification round (_verify_blocking_findings)
    ├── Candidate selection (see §2.2)
    ├── Per-finding: BedrockClient with HAIKU_MODEL_ID → "CONFIRMED" or "REFUTED"
    ├── CONFIRMED → promote confidence to max(current, block_threshold)
    └── REFUTED / error → set confidence to FIX_ISSUE_MIN_CONFIDENCE - 0.01

14. Suppression filtering
    ├── _fetch_suppressed_patterns(): bot-suppress-labeled closed issues
    ├── _fetch_repo_suppress_config(): .github/code-intelligence.yml suppress list
    └── _is_finding_suppressed(): substring OR ≥70% word overlap

15. Relevance gate (_relevance_gate_blocks)
    ├── min_findings check (repo config or PR_INTELLIGENCE_MIN_FINDINGS_TO_POST)
    └── suppress_advisory check (skip if APPROVE/APPROVE* with no required-change findings)

16. Output
    ├── DRY-RUN path:
    │   ├── PRReviewLog.write() → {owner}/{repo}/{pr_number}/review.{json,md}
    │   ├── _file_dry_run_calibration_issue() → GitHub issue with CALIBRATION_LABEL
    │   └── notify_pr_review_complete() → Slack
    └── LIVE path:
        ├── _post_review() → GitHub PR review comment
        ├── _file_or_update_tracker_issue() → upsert one tracker issue per PR
        ├── PRReviewLog.write()
        └── notify_pr_review_complete() → Slack
```

---

## 2. Verdict Policy and Verification Round

### 2.1 Fail-Safe Verdict Policy

**Default verdict is APPROVE. The bot bears the burden of proof, not the engineer.**

From the system prompt (`PR_REVIEW_SYSTEM_PROMPT`, lines ~960–980):

> REQUEST_CHANGES requires ALL THREE of the following from evidence in the visible diff:
> (a) a specific wrong line cited, (b) a traceable failure path, (c) a concrete fix.
>
> BLOCK = reserved for undisputed evidence of a serious flaw introduced by this PR.
>
> Default verdict is APPROVE. The bot bears the burden of proof, not the engineer.

Verdict taxonomy:
- `APPROVE` — merge as-is
- `APPROVE*` — merge, minor issues noted (no required-change findings)
- `REQUEST_CHANGES` — must fix before merge (real bug, contract violation, or safety risk demonstrated from visible diff)
- `BLOCK` — undisputed serious flaw (data loss, auth bypass, irreversible production breakage)

Grade patterns extracted by `_extract_review_grade()` from last 20% of review body using `_GRADE_PATTERNS` (list of compiled regexes). Letter grades A–F also supported (parsed by `None`-label entries in `_GRADE_PATTERNS`).

`_normalize_review_verdict()` normalizes raw extracted grade to one of: `APPROVE`, `APPROVE*`, `REQUEST_CHANGES`, `BLOCK`, or `N/A`.

### 2.2 Per-Finding Haiku Verification Round

**Function**: `_verify_blocking_findings()` (lines 4385–4615)

**When called**: After `_parse_fix_suggestions()`, before suppression filtering.

**Candidate selection** (changed 2026-05-30 — see Hard-won Lessons §7.1):

| Primary verdict | Candidates sent to verifier |
|---|---|
| `REQUEST_CHANGES` or `BLOCK` | ALL fix_suggestions with `confidence >= _VERIFY_CANDIDATE_MIN_CONFIDENCE` (0.50), sorted desc, capped at `FIX_ISSUE_MAX_PER_PR` (5) |
| `APPROVE`, `APPROVE*`, `N/A` | Only findings with `confidence >= block_threshold` (default 0.90) |

**Per-finding decision table**:

| Verifier response | Action |
|---|---|
| `CONFIRMED` | Promote confidence to `max(current, block_threshold)` |
| `REFUTED` | Set confidence to `FIX_ISSUE_MIN_CONFIDENCE - 0.01` (drops below both tiers) |
| Exception (any) | Same as REFUTED (fail toward APPROVE) |
| Truncation shortcut | Auto-REFUTE WITHOUT LLM call when diff is truncated AND both `finding.file` AND `finding.line` marker are absent from full diff text |

**Verifier model**: `HAIKU_MODEL_ID = "us.anthropic.claude-haiku-4-5-20251001-v1:0"` (imported from `diff_analyzer/hunk_classifier.py`; shared constant to avoid drift).

**Verification system prompt** (`_VERIFICATION_SYSTEM_PROMPT`): Requests exactly one word: `CONFIRMED` if specific wrong line is quotable and failure path is traceable entirely within visible code; `REFUTED` otherwise (absent evidence, requires assumptions outside diff, or diff appears truncated).

**Exception handling in verifier**: Bedrock errors classified by marker strings (`ResourceNotFoundException`, `ValidationException`, `AccessDeniedException`, `ModelNotReadyException`) → log `pr_review_verification_model_error` at ERROR level (distinguishes model-config errors from transient network errors). Both result in `confirmed = False`.

**Downgrade logic**: After verification, if primary verdict was `REQUEST_CHANGES`/`BLOCK` and NO surviving blocking findings remain (all refuted), the verdict in the review body is NOT rewritten (review body is returned unchanged) — but the filed findings list is empty, which means `APPROVE*` is the effective outcome.

### 2.3 Confidence Tiers

| Tier | Threshold | Behavior |
|---|---|---|
| Advisory | `FIX_ISSUE_MIN_CONFIDENCE` (0.70) | Summarized in review comment |
| Blocking / issue-eligible | `BLOCK_ISSUE_MIN_CONFIDENCE` (0.90) | Filed as tracker issue |
| `FIX_ISSUE_MAX_PER_PR` | 5 | Runaway safety cap on filed issues |
| `FIX_ISSUE_ALLOWED_EFFORTS` | `("low", "medium")` | Only these effort levels file issues |
| Repo-configurable `issue_threshold` | default 0.75 | Per-repo override |
| Repo-configurable `block_threshold` | default 0.90 | Per-repo override |
| Repo-configurable `pr_threshold` | default 0.95 | High-confidence flag threshold |

---

## 3. Diff Summarizer / DiffAnalyzer Pipeline

### 3.1 Stage Architecture (`services/diff_analyzer/`)

```
DiffAnalyzer.analyze(files, diff_text, config)
  ├── Stage A: FileFilter.apply() — deterministic file-level filter
  │   ├── Preserve-first: builtin preserve patterns (security/, .env, secrets/,
  │   │   config), credential-filename substrings (secret, token, password, key,
  │   │   dsn, credential), content-level credential URL regex, user preserve_patterns
  │   ├── Drop: lockfiles, snapshots, generated code, pure renames
  │   └── Summary-only: fixture/i18n files > fixture_summary_min_lines (30)
  │
  ├── Stage B: HunkFilter.apply() — regex hunk-level filter per surviving file
  │   ├── Whitespace-only hunks → WHITESPACE_ONLY
  │   ├── Import-only hunks (language-specific) → IMPORT_ONLY
  │   └── Comment-only hunks → COMMENT_ONLY
  │
  └── Stage C: HunkClassifier.classify() — Haiku per-hunk substantive scoring
      ├── Batches of BATCH_SIZE=10 hunks per Haiku call
      ├── mechanical + confidence > DROP_CONFIDENCE_THRESHOLD (0.7) → dropped
      │   UNLESS hunk contains credential URL (force-preserve)
      ├── substantive | uncertain → kept
      └── ANY Haiku error → ALL hunks in batch treated as "uncertain" (kept)
```

**Output**: `FilteredDiff.render_for_prompt(max_chars=12_000)` — no manifest header in output (framing-effect regression fix from forecasting#120; manifest is telemetry-only via `build_manifest_telemetry()`).

### 3.2 Hunk Classification Taxonomy

| Classification | Meaning | `is_droppable` |
|---|---|---|
| `substantive` | Logic, schema, API, security, control-flow change | False |
| `mechanical` | Formatting, whitespace, import reorder, generated, getters/setters | True if confidence > 0.7 |
| `uncertain` | Cannot determine without more context | False |

Haiku model: same `HAIKU_MODEL_ID` constant. Batch prompt requests JSON array with `{hunk_id, classification, confidence, reason}` per hunk. Temperature 0.0.

### 3.3 File/Hunk Data Models (`diff_analyzer/models.py`)

| Type | Key fields |
|---|---|
| `FilteredDiff` | `files: list[FilteredFile]`, `dropped_files: list[DroppedFile]`, `drop_hunk_counts: dict[str,int]`, `original_byte_size`, `filtered_byte_size`, `phase2_telemetry` |
| `FilteredFile` | `filename`, `status` (added/modified/renamed/removed), `disposition: FileDisposition`, `hunks: list[FilteredHunk]`, `dropped_hunks: list[DroppedHunk]`, `summary_line` |
| `FilteredHunk` | `header` (@@ line), `lines: list[str]`, `substantive_confidence`, `reason_kept` |
| `DroppedHunk` | `reason: HunkDropReason`, `lines_count`, `header` |
| `DroppedFile` | `path`, `reason` |
| `FileDisposition` | `KEPT`, `DROPPED`, `SUMMARY_ONLY` |
| `HunkDropReason` | `WHITESPACE_ONLY`, `IMPORT_ONLY`, `COMMENT_ONLY`, `MECHANICAL_HAIKU` |

### 3.4 Noisy File Patterns (in `pr_review_service.py`)

For PR diff display (separate from DiffAnalyzer), `NOISY_FILE_PATTERNS` and `NOISY_DIFF_MIN_LINES=30` suppress large fixture/i18n diffs: `.tsv`, `.csv`, `nls/**/*.{properties,json}`, `messages/**`, `labels/**`, `fixtures/**/*.{json,xml,sql}`, `test*fixtures*/**`, `__generated__/**`, `generated/**/*.{java,ts,js}`.

---

## 4. GitHub Integration (`pr_review_github.py`, `pr_review_service.py`)

### 4.1 Authentication

**GitHub App auth** (preferred over PAT):
- `GITHUB_APP_ID`, `GITHUB_APP_CLIENT_ID`, `GITHUB_APP_PRIVATE_KEY` (RSA)
- `GITHUB_INSTALLATION_ID_DUETTORESEARCH`, `GITHUB_INSTALLATION_ID_HOTSTATS`
- `_resolve_github_token(owner)` → selects installation token by org name
- Falls back to `GITHUB_TOKEN` (PAT) if App auth not configured

**Multi-org**: Two separate installation IDs for `duettoresearch` and `hotstats` orgs. Token selection is by `owner.lower()` match.

### 4.2 GitHub Operations Performed by the Bot

The bot is restricted to read + comment operations. `GH_ALLOW_PUSH = False` (hard-coded, not configurable). `_assert_no_push_operation()` raises `RuntimeError` if any of `_FORBIDDEN_PUSH_OPERATIONS` is called.

Permitted operations:
- POST PR review comment (`_post_review`)
- GET PR diff, file list, metadata
- GET/POST/PATCH issues (tracker + calibration issues)
- GET/POST labels
- POST issue type (REST numeric ID: `TASK_ISSUE_TYPE_NUMERIC_ID = 203410`)
- POST GraphQL for GitHub Project v2 add
- GET `.github/code-intelligence.yml`
- GET existing PR reviews (for Copilot detection)

### 4.3 Tracker Issue Semantics

One GitHub issue per PR (the "tracker"), updated in place on every re-review.

- Labels: `["code-review", "code-intelligence"]` (`TRACKER_ISSUE_LABELS`)
- Issue type: "Task" (`TASK_ISSUE_TYPE_ID = "IT_kwDOABbzg84AAxqS"` GraphQL node ID)
- Discovery label for Project #16 filter: `code-intelligence`
- Title pattern: `[PR Review] {owner}/{repo}#{pr_number} — {VERDICT}`
- Suppression pattern extracted from title: first 60 chars after marker (`SUPPRESS_PATTERN_PREFIX_LEN`)

**`_file_or_update_tracker_issue()`**: Searches existing `code-review`-labeled issues on the repo for a matching title prefix; updates body + title if found (upsert), creates if not found.

**Calibration issues** (dry-run): Filed with `CALIBRATION_ISSUE_LABEL = "code-intelligence-review"`, added to `PR Review Calibration` GitHub Project (created if missing). Body includes trigger, grade, PR link, and full review text.

### 4.4 Suppression Flow

1. `_fetch_suppressed_patterns()`: search repo issues for `bot-suppress` label + closed state → extract pattern from issue title via `_suppress_pattern_from_title()`
2. `_fetch_repo_suppress_config()`: fetch `.github/code-intelligence.yml`, return `suppress` list
3. Lists merged at review time
4. `_is_finding_suppressed(finding_desc, patterns)`: case-insensitive substring OR `_texts_similar()` (Jaccard word overlap >= `SUPPRESS_OVERLAP_THRESHOLD=0.7`)
5. ALL lookups fail-open (GitHub API error never blocks review)

---

## 5. Log and Calibration Tracker (`pr_review_log.py`)

### 5.1 `PRReviewResult` Data Model (full field list)

| Field | Type | Notes |
|---|---|---|
| `owner`, `repo` | str | GitHub org/repo |
| `pr_number` | int | PR number |
| `pr_title`, `pr_url` | str | |
| `review_body` | str | Full LLM markdown output |
| `analysis` | dict | Contains `grade`, `display_grade`, `display_grade_is_fallback`, `hotspots_in_pr`, `smells_in_pr`, `copilot_mode`, `bot_review_count` |
| `context_files` | list[str] | Filenames from vector search context |
| `input_tokens`, `output_tokens` | int | Final LLM call only |
| `cost_estimate_usd` | float | Estimated cost |
| `dry_run`, `posted` | bool | |
| `timestamp` | str | ISO timestamp |
| `error` | str\|None | Non-None on pipeline error |
| `fix_suggestions` | list[FixSuggestion] | |
| `filed_issue_numbers` | list[int] | GitHub issue numbers filed |
| `review_mode` | str | `"full"` or `"no_context"` |
| `head_sha` | str | For dedup; empty on pre-field records |
| `tracker_issue_number`, `tracker_issue_url` | int\|None, str\|None | |
| `diff` | str\|None | Stored diff for eval replay |
| `apex_context`, `jira_context`, `confluence_context`, `github_issues_context`, `cross_ref_context` | list[dict] | Slim metadata (bodies stripped via `_slim_context()`) |
| `copilot_mode` | str | `"full"` / `"supplement"` / `"duetto_only"` |
| `bot_review_count` | int | |
| `multipass` | bool | Two-pass pipeline flag |
| `pass1_tokens`, `pass2_tokens` | int | Multi-pass cost breakdown |
| `trigger` | str | `"manual"` or `"auto"` |
| `grade` | str | Top-level mirror of `analysis["grade"]` |
| `enrichment_rounds`, `enrichment_queries` | int, list[str] | Context enrichment audit |
| `skip_reason` | str\|None | Short token on pipeline skip |
| `review_version` | str | Pipeline version string (currently `"v1.4"`) |

### 5.2 `FixSuggestion` Data Model

| Field | Type | Notes |
|---|---|---|
| `file` | str | Changed file path |
| `line` | int\|None | Line number (optional) |
| `kind` | str | Finding type |
| `description` | str | Human-readable finding |
| `suggestion` | str | Proposed fix |
| `confidence` | float | 0.0–1.0 |
| `effort` | str | `"low"` / `"medium"` / `"high"` |

### 5.3 Directory Layout and Cross-Process Dedup

On-disk: `{PR_INTELLIGENCE_LOG_DIR}/{owner}/{repo}/{pr_number}/review.{json,md}`

Cross-process dedup: `{PR_INTELLIGENCE_LOG_DIR}/dedup.db` — SQLite with `in_flight_reviews` table (composite PK: `owner, repo, pr_number, head_sha`). `claim_review()` does atomic `INSERT OR FAIL`; stale claims (>2 hours) purged on each claim attempt. `release_review()` called in `finally` to free the claim.

---

## 6. Adapters

### 6.1 trusty-search Adapter (`adapters/vector_search.py`)

HTTP client for `TRUSTY_SEARCH_URL` (default `http://localhost:7878`). Index name from `TRUSTY_SEARCH_INDEX` (default `"main"`).

Key calls used by PR review:
- `POST /indexes/{index}/search` — semantic similarity search; query from PR title + changed file paths
- `GET /health` — liveness
- `GET /indexes/{index}/status` — file count / health

Multiple named `VectorSearchAdapter` instances used for different index targets:
- `vector_search` — main code index
- `knowledge_search` — JIRA/Confluence knowledge index (74K+ chunks)
- `apex_search` — APEX product spec index
- `github_search` — GitHub issues index

### 6.2 trusty-analyze Adapter (`adapters/analyzer_adapter.py`)

HTTP client for `PR_INTELLIGENCE_ANALYZER_URL` (default `http://localhost:7879`). Index from `TRUSTY_SEARCH_INDEX`.

| Method | Endpoint | Timeout | Notes |
|---|---|---|---|
| `health()` | `GET /health` | 5s (`_health_timeout`) | Liveness + `search_reachable` flag |
| `is_available()` | `GET /health` | 5s | Cached 60s TTL |
| `has_analysis()` | `GET /health` + `GET /indexes` | 5s each | Cheap readiness probe (see §8.2) |
| `complexity_hotspots()` | `GET /indexes/{id}/hotspots` | 180s | Returns list of hotspot dicts |
| `smells()` | `GET /indexes/{id}/smells` | 180s | Returns list of smell dicts |
| `quality_grade()` | `GET /indexes/{id}/quality` | 180s | O(corpus) — do NOT use as readiness probe |

All methods catch `httpx.ConnectError`, `TimeoutException`, `HTTPError` and return empty default. Non-2xx logged at WARNING (404 on known-optional paths at DEBUG).

### 6.3 Bedrock LLM Adapter (`adapters/bedrock_llm.py`)

Uses **Bedrock Converse API** (`bedrock-runtime.converse` / `converse_stream`). The Converse API normalizes request/response shape across model families.

| Setting | Value | Notes |
|---|---|---|
| Main review model | `us.anthropic.claude-sonnet-4-6` (from `BEDROCK_MODEL_ID`) | `us.` inference profile prefix required |
| Verifier/classifier model | `us.anthropic.claude-haiku-4-5-20251001-v1:0` | Must be `foundation_lifecycle=ACTIVE` |
| Region | `AWS_REGION` (default `us-east-1`) | |
| max_tokens | `bedrock.max_tokens` from settings | |
| temperature for review | 0.3 (webhook route) | Tighter than chat for determinism |
| temperature for verification | 1.0 default | Single-word response |
| Retry | `max_attempts=3, mode="adaptive"` | boto3 adaptive retry |
| Read timeout | 120s | |
| Token tracking | `_last_usage: dict[str, int]` | Set after each call; read by `_generate_review` |

Token metrics emitted to CloudWatch: `LLMInputTokens`, `LLMOutputTokens` tagged `ReviewType=PRReview`.

---

## 7. JIRA / Ticket Context Mechanism

### 7.1 `_fetch_jira_context()` (lines 4015–4085)

1. Extract ticket IDs matching `_JIRA_TICKET_RE = re.compile(r"\b([A-Z][A-Z0-9]+-\d+)\b")` from PR title + description
2. If ATLASSIAN credentials configured (`ATLASSIAN_EMAIL`, `ATLASSIAN_API_TOKEN`, `ATLASSIAN_URL`) and ticket IDs found → `_fetch_jira_tickets()`: `GET /rest/api/3/issue/{id}` per ticket (up to `MAX_KNOWLEDGE_RESULTS=3`)
3. If REST API returns nothing → `_vector_search_for_tickets()`: per-ticket keyword search against 74K+ JIRA/Confluence vector index
4. If no credentials → same vector search fallback
5. Final fallback: semantic search on PR title + description

### 7.2 APEX Context

APEX is treated as a separate vector index (`apex_search` adapter, queried via `SearchService`). The query is the same cross-query used for code context (PR title + changed filenames). Results up to `MAX_APEX_RESULTS=2`. Cited in review as `[apex: \`path/to/spec.md:15\` — "brief excerpt"]`.

**NEW vs today**: For trusty-review, apex must be treated as a repo (indexed alongside code repos in trusty-search) rather than a separate index. The spec author should design a single-index query that returns apex spec chunks alongside code chunks, or a configurable secondary index URL.

---

## 8. Per-Repo Config (`.github/code-intelligence.yml`)

### 8.1 Full Schema

| Key | Type | Default | Behavior |
|---|---|---|---|
| `skip` | bool | `false` | Disable all reviews for this repo |
| `issue_threshold` | float | `0.75` | Min confidence to file tracker issue |
| `block_threshold` | float | `0.90` | Min confidence for BLOCK tier |
| `pr_threshold` | float | `0.95` | Min confidence for high-confidence flag |
| `suppress` | list[str] | `[]` | Substring/word-overlap suppression patterns |
| `min_findings` | int | `1` | Min eligible findings to post to GitHub |
| `suppress_advisory` | bool | `false` | Skip posting APPROVE/APPROVE* with no required-change findings |
| `file_issues_on_merge_only` | bool | `false` | Defer tracker issue creation until PR merge |
| `diff_filter.suppress_patterns` | list[str] | `[]` | Additional file patterns to drop from diff |
| `diff_filter.preserve_patterns` | list[str] | `[]` | Force-keep patterns (override built-in drops) |
| `diff_filter.ignore_whitespace` | bool | `true` | Drop whitespace-only hunks |
| `diff_filter.ignore_imports` | bool | `true` | Drop import-only hunks |
| `diff_filter.ignore_comments` | bool | `true` | Drop comment-only hunks |
| `diff_filter.fixture_summary_min_lines` | int | `30` | Threshold for fixture file summarization |

**Validation**: `issue_threshold`, `block_threshold`, `pr_threshold` must be in `[0.0, 1.0]`; `pr_threshold >= issue_threshold`. Violations revert both to defaults. Unknown keys ignored. Config fails open (fetch/parse errors → defaults).

### 8.2 Suppression Matching

`_is_finding_suppressed(finding_desc, patterns)`:
- Case-insensitive
- OR condition: (a) pattern is substring of `finding_desc`, OR (b) `_texts_similar()` returns True
- `_texts_similar(a, b, threshold=FINDING_SIMILARITY_THRESHOLD=0.6)`: normalized word sets, Jaccard overlap >= threshold
- Suppression lookups always fail-open

---

## 9. Slack Notifications (`services/slack_notify.py`)

Two notification types relevant to PR review:

**`notify_pr_review_complete()`**: Single-line mrkdwn message with dry-run/live label, linked `owner/repo#PR`, verdict, findings count, optional calibration/tracker issue link. Never raises (Slack outage cannot break review).

**`notify_pr_review_service_failure()`**: Block Kit warning message when a required service (trusty-search, trusty-analyze, Bedrock) is unavailable. Includes service name, PR URL, truncated error (500 chars). Triggered by `PRReviewServiceUnavailable` exception caught in `_run_review_pipeline`.

Dispatch: prefers `SLACK_NOTIFICATION_CHANNEL` + `SLACK_BOT_TOKEN` (chat.postMessage); falls back to `SLACK_NOTIFICATION_WEBHOOK`.

---

## 10. Environment Variables / Configuration Reference

### 10.1 PR Intelligence Settings

| Env Var | Default | Effect |
|---|---|---|
| `PR_INTELLIGENCE_DRY_RUN` | `"true"` | `true` = log to disk; `false` = post to GitHub |
| `PR_INTELLIGENCE_ENABLED_REPOS` | `"*"` | Comma-sep allow-list; `"*"` = all; `"org/*"` = org wildcard |
| `PR_INTELLIGENCE_EXCLUDED_REPOS` | `""` | Comma-sep deny-list; always overrides allow-list |
| `PR_INTELLIGENCE_EXCLUDED_AUTHORS` | `""` | Bot logins to skip (e.g. `duetto-snyk-svc`) |
| `PR_REVIEW_BOT_USERNAME` | `""` | GitHub App login; triggers manual/live mode when requested as reviewer |
| `PR_INTELLIGENCE_LOG_DIR` | `/data/app/data/pr-reviews` | Dry-run log directory |
| `PR_INTELLIGENCE_ANALYZER_URL` | `http://localhost:7879` | trusty-analyze sidecar URL |
| `PR_INTELLIGENCE_EVAL_MODEL` | (varies) | Secondary model for eval comparisons (dry-run only) |
| `PR_INTELLIGENCE_MIN_FINDINGS_TO_POST` | `1` | Global min findings gate (overrideable per-repo) |
| `PR_INTELLIGENCE_SUPPRESS_ADVISORY_REVIEWS` | `false` | Global suppress_advisory flag |
| `PR_INTELLIGENCE_FILE_ISSUES_ON_MERGE_ONLY` | `false` | Global file_issues_on_merge_only flag |

### 10.2 GitHub App Settings

| Env Var | Effect |
|---|---|
| `GITHUB_APP_ID` | Numeric app ID |
| `GITHUB_APP_CLIENT_ID` | App OAuth client ID |
| `GITHUB_APP_PRIVATE_KEY` | RSA private key (PEM, newlines as `\n`) |
| `GITHUB_INSTALLATION_ID_DUETTORESEARCH` | Installation ID for duettoresearch org |
| `GITHUB_INSTALLATION_ID_HOTSTATS` | Installation ID for hotstats org |
| `GITHUB_WEBHOOK_SECRET` | HMAC secret for webhook signature verification |
| `GITHUB_TOKEN` | PAT fallback when App auth not configured |

### 10.3 LLM and Infrastructure

| Env Var | Default | Effect |
|---|---|---|
| `BEDROCK_MODEL_ID` | `us.anthropic.claude-sonnet-4-6` | Main review LLM |
| `AWS_REGION` | `us-west-2` | Bedrock region |
| `TRUSTY_SEARCH_URL` | `http://localhost:7878` | trusty-search HTTP endpoint |
| `TRUSTY_SEARCH_INDEX` | `"main"` | Index name for both trusty-search and trusty-analyze |
| `ATLASSIAN_URL` | `""` | Jira/Confluence base URL |
| `ATLASSIAN_EMAIL` | `""` | API auth email |
| `ATLASSIAN_API_TOKEN` | `""` | API token |
| `SLACK_NOTIFICATION_WEBHOOK` | `""` | Slack webhook for notifications |
| `SLACK_NOTIFICATION_CHANNEL` | `""` | Slack channel (preferred over webhook) |
| `SLACK_BOT_TOKEN` | `""` | Bot token for channel posting |

---

## 11. trusty-tools Workspace Conventions for trusty-review

### 11.1 Workspace Structure

```
trusty-tools/
├── Cargo.toml          # workspace root; glob members = ["crates/*"]
├── crates/
│   ├── trusty-common/  # shared libs (chat, MCP, embedder, memory-core, etc.)
│   ├── trusty-search/  # HTTP daemon :7878; lib + binary
│   └── trusty-analyze/ # HTTP daemon :7879; lib + binary
```

The new `trusty-review` crate should follow the same pattern: `crates/trusty-review/` with a `lib.rs` and a `main.rs` (`[[bin]]`).

### 11.2 Crate Conventions (from `CLAUDE.md`)

| Rule | Detail |
|---|---|
| Why/What/Test doc comments | Every public item must have all three sections |
| 500-line file cap | Split files proactively; no file > 500 lines |
| `thiserror` for libraries | Structured error enums; `anyhow` for binary crates |
| No `unwrap()` in library code | Use `?` or `expect()` for true invariants only |
| No global state | Free functions or small structs; no `lazy_static!` |
| Logs to stderr | Never stdout (MCP JSON-RPC framing uses stdout) |
| `axum` behind `http-server` feature | Gate axum/tower-http so library consumers don't pull in HTTP stack |
| Edition 2021 | (2024 only for `trusty-mpm`, `open-mpm` family) |
| Workspace dependencies | All shared deps declared once in root `[workspace.dependencies]` |
| MSRV 1.88 | Required for let-chains |

### 11.3 HTTP Server Pattern (trusty-analyze as reference)

`trusty-analyze/src/service/mod.rs` exposes `build_router(state: AnalyzerAppState) -> Router` returning an axum 0.8 router. Routes follow the pattern:

```rust
Router::new()
    .route("/health", get(health))
    .route("/indexes", get(list_indexes))
    .route("/indexes/{id}/complexity_hotspots", get(complexity_hotspots))
    // ...
```

The binary (`main.rs`) resolves config, creates state, calls `build_router()`, and binds with `axum::serve()`.

### 11.4 LLM Client in trusty-tools

`trusty-common` provides `OpenRouterProvider` (via `chat` module) using the OpenRouter API (`OPENROUTER_URL = "https://openrouter.ai/api/v1/chat/completions"`). The `OPENROUTER_API_KEY` env var provides auth. Default model: `anthropic/claude-haiku-4.5`.

**NEW vs today**: trusty-review should implement a `LlmProvider` trait with at minimum two concrete backends:
1. `OpenRouterProvider` (via `trusty-common::chat`) — for local/standalone use
2. `BedrockProvider` — for production Duetto deployment

The provider must be selectable at runtime (env var or config), not compile-time. The verifier (Haiku-tier) and main review (Sonnet-tier) should each accept a `Box<dyn LlmProvider>` or equivalent.

### 11.5 trusty-search and trusty-analyze HTTP APIs (consumed by trusty-review)

**trusty-search (:7878)**:

| Endpoint | Method | Purpose |
|---|---|---|
| `/health` | GET | Liveness; returns `embedder` status, `search_reachable` |
| `/indexes` | GET | List registered indexes |
| `/indexes/:id/status` | GET | Chunk count + root path |
| `/indexes/:id/search` | POST | Hybrid BM25+vector search |
| `/indexes/:id/reindex` | POST | Fire-and-forget reindex |

**trusty-analyze (:7879)**:

| Endpoint | Method | Purpose |
|---|---|---|
| `/health` | GET | Liveness + `search_reachable` flag |
| `/indexes` | GET | List indexes (cheap readiness probe step 2) |
| `/indexes/{id}/complexity_hotspots` | GET | Top-N complexity hotspots |
| `/indexes/{id}/smells` | GET | Code smell findings |
| `/indexes/{id}/quality` | GET | Aggregate quality report (O(corpus) — do NOT use as probe) |

Both are consumed over HTTP (not in-process). trusty-review should have its own HTTP client modules for each, analogous to the Python adapters, with the same graceful degradation semantics.

---

## 12. Hard-Won Lessons / Failure Modes

### 12.1 Inactive Verifier Model → Silent All-Approve Degradation

**Symptom**: All blocking findings were being auto-refuted silently; every PR review effectively became APPROVE regardless of the diff content.

**Root cause**: `_VERIFICATION_MODEL_ID` was hardcoded to `"us.anthropic.claude-3-5-haiku-20241022-v1:0"` which had `foundation_lifecycle=LEGACY` on the Duetto Bedrock account. Every verification call threw `ResourceNotFoundException`. The exception handler set `confirmed = False`, which meant REFUTED for every finding. With all findings refuted, the downgrade logic produced APPROVE.

**Fix**: The constant is now sourced from `hunk_classifier.HAIKU_MODEL_ID = "us.anthropic.claude-haiku-4-5-20251001-v1:0"` — the only Haiku tier with `foundation_lifecycle=ACTIVE`. The error is now classified at ERROR level (`pr_review_verification_model_error` log event) to distinguish model-config failure from transient network errors.

**NEW vs today implication**: trusty-review must alarm on `verification_model_error` events. An inactive model ID must produce an observable alert, not a silent degradation. The LLM provider abstraction must distinguish "model not found" errors from transient failures.

### 12.2 Bedrock Inference Profile ID Requirement

**Symptom**: Bedrock calls fail with `ValidationException` or `ResourceNotFoundException` when using bare model IDs like `anthropic.claude-3-5-sonnet-20241022-v2:0`.

**Root cause**: Cross-region inference profiles require the `us.` prefix (e.g., `us.anthropic.claude-sonnet-4-6`). Without the prefix, Bedrock rejects the model ID for cross-region invocation.

**Fix**: All model IDs in production configuration use the `us.` inference profile prefix.

**NEW vs today implication**: trusty-review's OpenRouter provider avoids this — OpenRouter uses model slugs like `anthropic/claude-haiku-4.5`. If a Bedrock provider is implemented, it must enforce the `us.` prefix or accept it as a required configuration input.

### 12.3 Analyzer Readiness Probe False "corpus_not_ready"

**Symptom**: Every PR review logged `analysis.available: False, reason: trusty_analyze_corpus_not_ready` even when trusty-analyze was running correctly.

**Root cause**: `has_analysis()` was probing `GET /indexes/{id}/quality` with a 5-second timeout. The `/quality` endpoint is O(corpus) — it streams all ~396k chunks from trusty-search and can take 11–103 seconds. Every probe timed out, making the sidecar appear unavailable.

**Fix**: `has_analysis()` now uses two cheap checks: (1) `GET /health` (O(1), liveness + `search_reachable`), (2) `GET /indexes` (O(n_indexes), typically 1–3 indexes). Both use `_health_timeout=5s`. The `/quality` endpoint is never called as a readiness probe.

**NEW vs today implication**: trusty-review must replicate this two-step probe. Never use an O(corpus) endpoint as a readiness check. The trusty-analyze daemon's `/health` response includes a `search_reachable` boolean flag — check it explicitly.

### 12.4 Concurrent Webhook Delivery → Duplicate Reviews

**Symptom**: GitHub fires several distinct webhook events for one PR push (e.g., `synchronize` + `review_requested` within seconds). Without guards, both events ran the full LLM pipeline and produced duplicate calibration issues.

**Fix**: Three-layer dedup:
1. Webhook-level filter: only `review_requested` triggers dispatch (non-`review_requested` actions return 200 OK but no dispatch)
2. `_PR_IN_FLIGHT` set: in-process per-(owner,repo,pr_number) guard active from request receipt (before SHA is known)
3. `_IN_FLIGHT` set + SQLite `claim_review()`: per-(owner,repo,pr_number,head_sha) cross-process atomic claim

**NEW vs today implication**: trusty-review needs all three layers. The SQLite cross-process claim is critical for multi-worker deployments. The `claim_review/release_review` in-`finally` pattern must be preserved to avoid leaked claims.

### 12.5 Verifier Candidate Selection Bug (Sub-0.90 Findings Never Verified)

**Symptom**: Real defects with confidence 0.65–0.82 were never reaching the verifier. The LLM would write `REQUEST_CHANGES` in its narrative but assign the supporting finding a confidence below 0.90. Those findings were excluded from the verify-only-blocking-tier path. After verification (which had no candidates), the downgrade logic fired and produced APPROVE*.

**Root cause**: The original candidate selection for the verifier only included findings with `confidence >= block_threshold` (0.90). Sub-threshold findings were never verified even when the primary verdict was REQUEST_CHANGES.

**Fix** (2026-05-30): When primary verdict ∈ {REQUEST_CHANGES, BLOCK}, verify ALL findings with `confidence >= _VERIFY_CANDIDATE_MIN_CONFIDENCE` (0.50), sorted desc, capped at 5. Only APPROVE/APPROVE*/N/A verdicts still use the old blocking-only selection.

### 12.6 Verifier Truncation Shortcut Over-Eagerness

**Symptom**: Real findings at line ~5000 chars in the diff were being auto-refuted without an LLM call. The truncation shortcut checked whether the finding's location was present in a 4k-char window, not the full diff.

**Root cause**: The truncation shortcut was checking the 4k-char verification slice (not the full diff). A finding whose line appeared at char 4001–16384 was marked "absent from diff" and auto-refuted.

**Fix**: `diff_for_verify` is now the full `diff_text` (already bounded to `MAX_DIFF_CHARS` by the caller), not a smaller sub-slice. The truncation shortcut only fires when BOTH the file path AND the line marker are absent from the FULL diff — genuinely hallucinated locations.

### 12.7 Calibration Tracker Upsert-Per-PR

**Symptom**: Each re-push to a PR created a new tracker issue instead of updating the existing one, accumulating 8+ duplicate issues per active PR.

**Fix**: `_file_or_update_tracker_issue()` searches for an existing issue with the stable `code-review` label and matching title prefix before creating a new one. First-push creates; subsequent pushes update body + title in place.

### 12.8 Dry-Run Rollout Discipline

**Symptom** (operational risk): Premature switch to live mode before the review quality was validated.

**Design**: `PR_INTELLIGENCE_DRY_RUN=true` is the default. In dry-run mode, the pipeline runs fully (diff fetch, context retrieval, LLM review, verification) but posts nothing to GitHub — writes only to disk and files a calibration GitHub issue. The webhook handler further enforces that only `review_requested` events (not `opened`/`synchronize`) trigger dispatch, regardless of `PR_INTELLIGENCE_DRY_RUN`.

**NEW vs today implication**: trusty-review should implement the same dry-run default. Flip to live only when `--live` or `dry_run=false` is explicitly set.

### 12.9 Suppression Fail-Open

**Design principle**: Every suppression lookup (GitHub API for `bot-suppress` labels, `.github/code-intelligence.yml` fetch) wraps in try/except and returns an empty list on error. A GitHub API error during suppression never blocks the review from running with unsuppressed findings.

**NEW vs today implication**: trusty-review must implement the same fail-open contract. HTTP errors fetching config/labels → treat as no suppression rules.

### 12.10 Graceful Degradation When trusty-analyze Is Down

**Design**: `AnalyzerAdapter.has_analysis()` returns `False` when the sidecar is down. `_analyze_changed_files()` checks this and returns an empty analysis dict. The review proceeds with no static analysis context — the LLM still runs on diff + vector search context.

`PRReviewServiceUnavailable` is raised only for the three required services: trusty-search, AWS Bedrock. trusty-analyze failure is NOT raised as `PRReviewServiceUnavailable` — it degrades silently.

**NEW vs today implication**: trusty-review must distinguish optional (trusty-analyze) from required (trusty-search, LLM) dependencies. Optional service failures degrade gracefully; required service failures skip the review entirely and Slack-notify.

### 12.11 Push Firewall (Learned from Production Incident)

**Symptom**: The bot was caught force-pushing to an infrastructure repo.

**Fix**: `GH_ALLOW_PUSH = False` is hard-coded. `_assert_no_push_operation()` raises `RuntimeError` on any attempted git-write operation. The constant is intentionally not configurable via env vars or per-repo settings.

**NEW vs today implication**: trusty-review must implement an equivalent firewall. The bot is read + comment only. No branch creation, file commits, or PR creation against reviewed repos.

### 12.12 Noisy Fixture/Generated File Diff Problem

**Symptom**: PRs touching large i18n label files or generated sources buried the meaningful code change in column-noise. The bot saw the fixture churn, burned most of its context budget on it, and missed the real change (demonstrated with PR #9545).

**Fix**: `NOISY_FILE_PATTERNS` collapses large diffs of fixture/i18n files to a 1-line summary. The DiffAnalyzer Stage A/B/C pipeline additionally drops lockfiles, snapshots, generated code, whitespace/import/comment-only hunks.

### 12.13 Cross-Reference Blast-Radius Gap

**Symptom**: PR #9545 deleted a permission check. The value it gated was still enqueued by a producer in another file the PR never touched. A similarity search keyed on the diff alone never surfaced the orphaned producer.

**Fix**: `_search_cross_references()` extracts enum constants (`ALL_CAPS_WITH_UNDERSCORE`) and method calls (`camelCase(`) from deleted lines, searches for other files that reference them but were NOT changed, and prioritizes producer-keyword hits (`enqueue`, `schedule`, `produce`, etc.).

---

## 13. NEW vs Today — Delta Flags for Spec Author

| Dimension | Today (Python) | NEW (trusty-review Rust) |
|---|---|---|
| Language | Python 3.11+, FastAPI | Rust, axum 0.8 |
| LLM provider | AWS Bedrock only (Converse API) | Co-equal Bedrock + OpenRouter; provider abstracted via trait |
| Model selection | Fixed by env var | Configurable per call-type (main vs verifier) |
| JIRA context | ATLASSIAN_* REST API + vector search fallback | Same logic; configurable JIRA REST client |
| APEX context | Separate VectorSearchAdapter instance | Apex treated as a repo; indexed alongside code in trusty-search; same index query |
| Deployment | FastAPI app (embedded in code-intelligence) | Standalone Rust service; can run as server or CLI |
| Dedup storage | SQLite (`dedup.db`) | Redb (aligns with trusty-analyze/trusty-search) or SQLite; TBD in spec |
| Log storage | Filesystem JSON/MD | Filesystem JSON/MD (same schema); or pluggable backend |
| GitHub auth | PyGithub + httpx + GithubIntegration | reqwest + octocrab or raw HTTP; GitHub App JWT generation in Rust |
| Suppression matching | Python regex + word overlap | Same algorithm in Rust |
| Diff analyzer | Python + Haiku LLM | Rust deterministic stages + LLM stage via provider trait |
| Webhook server | FastAPI route | axum route (matches trusty-analyze/trusty-search pattern) |
| Multi-org | INSTALLATION_ID_* per org | Same; config-driven |
