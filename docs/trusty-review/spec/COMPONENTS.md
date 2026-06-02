# trusty-review — Components

> **Status:** Current (v0.1.0 · 2026-06-01)
> **Derived from:** spec docs 02, 03, 04, 05, 06, 08 + source audit

---

## 1. Review pipeline (`src/pipeline/`)

**Purpose:** Ordered orchestration of a PR review from diff fetch through verdict output. Runs identically in server and CLI modes; only the trigger and output side-effects differ.

**Entry point:** `pipeline::runner::run_review(config, input, deps) -> ReviewResult`

**Submodules:**

| Module | Responsibility |
|--------|----------------|
| `diff.rs` | `DiffSource` enum (Github / LocalFile), diff loading, `MAX_DIFF_CHARS` = 60,000 char truncation, identifier extraction for enrichment |
| `grade.rs` | `derive_verdict(model_proposed, findings)` — severity-anchored deterministic grade derivation with low-confidence override and severity floor (see ARCHITECTURE §5) |
| `prompt.rs` | `reviewer_system_prompt()`, `build_review_prompt()`, `ReviewContext`, `ReviewPrMeta` |
| `parser.rs` | `parse_review_response() -> ParsedReview` — verdict token extraction + `Finding` JSON block parsing from LLM response body |
| `output.rs` | `write_review_log()` (filesystem JSON + Markdown), `print_review_result()` (stdout), `log_json_path()` |
| `context_gate.rs` | `preflight_context()`, `GateOutcome`, `degraded_banner()` — required-context gate (#590); see §1.1 |
| `runner.rs` | `ReviewDeps`, `ReviewInput`, `run_review()` — top-level orchestration loop |

**Pipeline stages (implemented):**

1. Diff fetch or local-file read → truncation to `MAX_DIFF_CHARS`
2. **Required-context gate** (`preflight_context()`, §1.1) — skip/degrade decision
3. Prompt assembly (PR meta + diff + context)
4. Reviewer LLM call (forced structured JSON output via Bedrock tool-use / OpenRouter)
5. `parse_review_response()` → `ParsedReview` (verdict token + findings)
6. `derive_verdict()` — severity floor + low-confidence override
7. Log write + stdout print

### 1.1 Required-context gate (`context_gate.rs`, #590)

trusty-review's value IS the context it injects from trusty-search (code context)
and trusty-analyze (static analysis); a context-free review gives false
confidence and is actively harmful. Both daemons are therefore **REQUIRED by
default**. Before context is gathered, `preflight_context(config, deps)` probes
both concurrently and returns a `GateOutcome`:

| `GateOutcome` | Trigger | Effect on `run_review` |
|---------------|---------|------------------------|
| `Proceed` | both reachable | normal authoritative review (`status = Completed`) |
| `Skip(reason)` | a REQUIRED dep is unavailable | `ReviewResult { status: Skipped, verdict: Unknown, error: Some(reason), dry_run: true }`, returned **before** the LLM call and **without** finalize/post. The CLI/`run` path turns this into a non-zero exit (`anyhow::Err`); `serve`/`POST /review`/webhook return it as a structured skip result and never post. |
| `Degraded(reason)` | a dep is unavailable but opted-out (`require_*=false`) | review proceeds but `status = Degraded`, a `⚠️ DEGRADED REVIEW — NOT AUTHORITATIVE` banner is prepended to `review_body`, and `error` records the reason |

The gate lives in the shared pipeline, so it applies uniformly to PR review,
local-diff review, and the forward-compatible commit-review (#589). Config knobs:
`require_search` / `require_analyze` (default `true`), layered
env (`TRUSTY_REVIEW_REQUIRE_SEARCH` / `TRUSTY_REVIEW_REQUIRE_ANALYZE`) over TOML
`[context]` over default — see §7.1.

**Pipeline stages (designed, not yet built in v0.1):**

- Stages 1–16 full sequence (eligibility check, three-layer dedup, repo-config load, copilot mode detection, head-SHA dedup, parallel context retrieval, iterative enrichment, static analysis, per-finding verification round, suppression filtering, relevance gate, live GitHub posting).
- The current runner is a focused MVP: diff → LLM → grade → output. Full 16-stage pipeline is tracked in issue #552.

**Key requirements:**
- Fail-safe default: pipeline errors → `Verdict::Approve` (never BLOCK from pipeline failure).
- `Verdict::Unknown` preserved when the model signals the diff was unassessable.
- `MAX_DIFF_CHARS` = 60,000 (impl) vs spec's 16,384 (design target; current code uses a higher value reflecting real PR sizes).

---

## 2. Grade derivation (`src/pipeline/grade.rs`)

**Purpose:** Deterministic post-processing of the model's proposed verdict to correct systematic under-firing.

**`derive_verdict(model_proposed: Verdict, findings: &[Finding]) -> Verdict`**

Two-pass algorithm:
1. **Low-confidence override:** If all findings have `confidence ≤ 0.65` AND none are `Effort::High` → force `APPROVE`. Prevents `APPROVE*` over-fire on speculative advisory findings.
2. **Severity floor:** Compute `severity_floor(findings)` and return `max(model_proposed, floor)`. The floor is `BLOCK` for any `High`-effort finding, `REQUEST_CHANGES` for ≥2 `Medium`, `APPROVE*` for exactly 1 `Medium`, `APPROVE` otherwise.

`Verdict::Unknown` always passes through unchanged.

Calibration basis: 0% BLOCK detection and REQUEST_CHANGES only 36% of the time on 30 live PRs without the floor rules.

---

## 3. Structured JSON output (`src/llm/bedrock/tool_use.rs`)

**Purpose:** Force the LLM to return structured JSON review output by using Bedrock's tool-use API, eliminating free-form parsing fragility.

The reviewer call uses a defined tool schema so the model must return a valid JSON object with `verdict`, `grade`, `findings[]`, and `review_body` fields. The OpenRouter path uses the same schema via the tool-use equivalent in the OpenAI-compatible API.

---

## 4. LLM provider layer (`src/llm/`)

**Purpose:** Pluggable LLM provider abstraction with co-equal Bedrock and OpenRouter backends. Per-run and per-role model selection.

### 4.1 Provider trait

```
trait LlmProvider (Send + Sync):
    async fn complete(req: LlmRequest) -> Result<LlmResponse, LlmError>
    fn name() -> &str   // "bedrock" | "openrouter"

LlmRequest:
    model: String       // resolved model ID for this call
    system: String
    messages: Vec<ChatMessage>
    temperature: f32
    max_tokens: u32

LlmResponse:
    text: String
    input_tokens: u32
    output_tokens: u32
    cost_usd: f64
    latency_ms: u64
    model: String
```

**`LlmError` (thiserror enum) — error classification is critical:**

| Variant | Retry? | Action |
|---------|--------|--------|
| `ModelNotFound` | No | ALARM: `verification_model_error` |
| `ModelNotReady` | No | ALARM: `verification_model_error` |
| `Validation(String)` | No | ALARM: `verification_model_error` |
| `AccessDenied` | No | ALARM: `verification_model_error` |
| `Transport(String)` | Yes (≤3) | Retry with backoff |
| `RateLimited` | Yes | Retry with backoff |
| `Upstream { status, body }` | If 5xx | Retry with backoff |

Config/lifecycle errors must not be retried — they signal a broken configuration that retries cannot fix. An inactive verifier model that triggers `ModelNotFound` must alarm immediately.

### 4.2 Three roles, independently configurable

| Role | Default model | Default provider | Temperature | Used by |
|------|--------------|-----------------|-------------|---------|
| **reviewer** | `us.anthropic.claude-sonnet-4-6` | Bedrock | 0.3 | Main LLM review pass |
| **verifier** | `us.anthropic.claude-haiku-4-5-20251001-v1:0` | Bedrock | 1.0 | Per-finding verification round (not yet in v0.1) |
| **summarizer** | `us.anthropic.claude-haiku-4-5-20251001-v1:0` | Bedrock | 0.0 | Diff Stage C classifier (not yet in v0.1) |

Each role may use a different provider (e.g. reviewer on Bedrock, summarizer on OpenRouter). Resolution precedence: CLI flag → per-role env var → config file `[models]` table → built-in default.

### 4.3 BedrockProvider

- Uses the AWS Bedrock Converse API (`converse`).
- Auth: standard AWS credential chain (env vars, `~/.aws/credentials`, IAM roles, SSO). No API key needed.
- Region: `AWS_REGION` env var (default `us-east-1`).
- **Model IDs must carry the cross-region inference-profile prefix** (`us.`). A bare ID (e.g. `anthropic.claude-3-5-sonnet-...`) fails with `ValidationException`. This is validated at config-resolution time.
- Per-call timeouts: connect 10s, read 120s. Max 3 retry attempts on transient errors.
- Cost tracked via a per-model pricing table in `llm/bedrock/pricing.rs`.

**Verified Bedrock model IDs (June 2026):**

| Role | Model ID |
|------|----------|
| Reviewer | `us.anthropic.claude-sonnet-4-6` (no date stamp needed) |
| Verifier / Summarizer | `us.anthropic.claude-haiku-4-5-20251001-v1:0` (date-versioned; the short form `us.anthropic.claude-haiku-4-5` produces HTTP 400) |
| Compare Sonnet 4.5 | `us.anthropic.claude-sonnet-4-5-20250929-v1:0` (confirmed available) |

**Default compare-set** (`COMPARE_CANDIDATE_MODELS`, all `bedrock/`-prefixed):
1. `bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0` (cheapest)
2. `bedrock/us.anthropic.claude-sonnet-4-5-20250929-v1:0` (mid-tier)
3. `bedrock/us.anthropic.claude-sonnet-4-6` (reviewer default)

Opus is intentionally excluded (not access-granted in the target account).

### 4.4 OpenRouterProvider

- Uses OpenAI-compatible `/v1/chat/completions` via `trusty_common::chat::OpenRouterProvider` (reuse, not re-implemented).
- Auth: `OPENROUTER_API_KEY` env var.
- Model IDs use OpenRouter slugs (e.g. `anthropic/claude-haiku-4.5`). No `us.` prefix — that is Bedrock-specific.
- Roles may use `openrouter/<model-slug>` prefix on CLI flags to force OpenRouter regardless of default provider.

### 4.5 Provider selection

`build_provider(model_slug, default_provider, openrouter_api_key)` resolves the provider:
- If `model_slug` starts with `bedrock/` → strip prefix, use `BedrockProvider`.
- If `model_slug` starts with `openrouter/` → strip prefix, use `OpenRouterProvider`.
- Otherwise → use `default_provider` from config/env.

---

## 5. Data models (`src/models/mod.rs`)

**`Verdict`** — serialised as exact board strings:
- `APPROVE` — no significant concerns
- `APPROVE*` — minor advisory notes; no blocking required-change findings
- `REQUEST_CHANGES` — at least one real correctness/logic change required before merge
- `BLOCK` — critical flaw (build-breaking, data-corrupting, auth-bypassing)
- `UNKNOWN` — diff too truncated or insufficient context to assess; NOT a clean APPROVE

**`Effort`** — proxy for finding severity:
- `Low` — advisory; no required change
- `Medium` — should fix; eligible for tracker-issue filing
- `High` — critical; maps to BLOCK floor in grade derivation

**`VerifyOutcome`** — per-finding verification result:
- `Confirmed` — verifier LLM confirmed finding is real
- `Refuted` — verifier LLM refuted finding
- `ErrorRefuted { error_class }` — config/lifecycle error; emits `verification_model_error` alarm
- `TruncationRefuted` — finding location absent from full diff
- `Skipped` — below verification confidence threshold

**`Finding`** — a single code-review finding:
- `file: String`, `line: Option<u32>`, `kind: String`, `description: String`, `suggestion: String`
- `confidence: f32` (clamped to [0.0, 1.0] at construction; defensive against malformed LLM JSON)
- `effort: Effort`, `verified: Option<VerifyOutcome>`, `issue_eligible: bool`

**`ReviewStatus`** (`src/models/status.rs`, #590) — review-run outcome class:
- `Completed` (default) — ran with full required context; verdict is **authoritative**
- `Skipped` — a REQUIRED context dep (trusty-search/trusty-analyze) was unavailable; **no real verdict** was produced (CLI exits non-zero; service never posts)
- `Degraded` — opted-in context-free run; the verdict is present but explicitly **non-authoritative** (loudly labelled in `review_body`)
- helpers: `is_authoritative()` (only `Completed`), `is_skipped()`

**`ReviewResult`** — complete output of a PR review pass:
- PR identity: `owner`, `repo`, `pr_number`, `pr_title`, `pr_url`
- Review output: `review_body`, `verdict`, `findings: Vec<Finding>`
- Telemetry: `model`, `input_tokens`, `output_tokens`, `cost_estimate_usd`, `latency_ms`
- Pipeline flags: `status: ReviewStatus` (default `Completed`, #590), `dry_run` (default true), `posted` (default false), `timestamp`
- Dedup: `head_sha`
- Versioning: `review_version` (currently `"tr-0.1"`)

**On-disk schema** (filesystem log):
```
{PR_INTELLIGENCE_LOG_DIR}/{owner}/{repo}/{pr_number}/
  review.json   — serialised ReviewResult (canonical schema)
  review.md     — human-readable review_body + header
```

JSON schema is additively versioned via `review_version`. Existing field names are not renamed for back-compat with Python log tooling.

**Confidence thresholds** (`src/config/constants.rs`):

| Constant | Value | Purpose |
|----------|-------|---------|
| `FIX_ISSUE_MIN_CONFIDENCE` | 0.60 | Minimum to include a finding in review body |
| `BLOCK_ISSUE_MIN_CONFIDENCE` | 0.75 | Minimum to file a tracker issue |
| `BLOCK_VERDICT_MIN_CONFIDENCE` | 0.90 | Minimum for BLOCK-tier verdict candidacy |
| `VERIFY_CANDIDATE_MIN_CONFIDENCE` | 0.95 | High-confidence PR flag threshold |
| `VERIFICATION_MIN_CONFIDENCE` | 0.65 | Minimum to include in verification round |
| `SUPPRESS_OVERLAP_THRESHOLD` | 0.70 | Jaccard overlap threshold for suppression matching |
| `DEDUP_STALE_SECS` | 7200 | Seconds after which a dedup claim is considered stale |
| `MAX_DIFF_CHARS` | 60,000 | Max diff text characters fed to LLM |
| `MAX_CONTEXT_FILES` | 20 | Max context files from trusty-search per review |
| `MAX_ENRICHMENT_ROUNDS` | 3 | Max iterative enrichment rounds |
| `FIX_ISSUE_MAX_PER_PR` | 3 | Max tracker issues filed per PR |

---

## 6. Integration clients (`src/integrations/`)

### 6.1 GitHub client (`integrations/github/`)

**Auth** (`auth.rs`): GitHub App preferred (JWT minted via `jsonwebtoken`, exchanged for short-lived installation token), PAT fallback. Multi-org: `resolve_token(owner)` selects installation token by case-insensitive owner match (`duettoresearch` vs `hotstats`), falling back to `GITHUB_TOKEN`.

**Push firewall** (`firewall.rs`): Hard-coded `GH_ALLOW_PUSH = false`. `assert_no_push_operation()` panics/errors on any git-write attempt. Not configurable by env or per-repo config.

**Permitted operations** (`pr.rs`): fetch PR diff, file list, metadata; POST PR review comment; GET/POST/PATCH issues; GET `.github/code-intelligence.yml`; GET existing PR reviews.

**Webhook verification** (`webhook.rs`): `X-Hub-Signature-256` HMAC validation using `GITHUB_WEBHOOK_SECRET`. Constant-time comparison.

**Env vars:**

| Variable | Purpose |
|----------|---------|
| `GITHUB_APP_ID` | App numeric ID |
| `GITHUB_APP_CLIENT_ID` | App OAuth client ID |
| `GITHUB_APP_PRIVATE_KEY` | RSA PEM (`\n`-escaped newlines) |
| `GITHUB_INSTALLATION_ID_DUETTORESEARCH` | duettoresearch org installation |
| `GITHUB_INSTALLATION_ID_HOTSTATS` | hotstats org installation |
| `GITHUB_WEBHOOK_SECRET` | HMAC secret for `X-Hub-Signature-256` |
| `GITHUB_TOKEN` | PAT fallback |

**Tracker issue semantics:** One GitHub issue per PR, upserted on each re-review. Labels: `["code-review", "code-intelligence"]`. Title pattern: `[PR Review] {owner}/{repo}#{pr_number} — {VERDICT}`. Search by `code-review`-labeled issues with matching title prefix; update in place if found, create if absent. (Not yet implemented in v0.1 pipeline runner — tracked in #552.)

### 6.2 trusty-search client (`integrations/search_client.rs`)

`HttpSearchClient` over `TRUSTY_SEARCH_URL` (default `http://localhost:7878`), index from `TRUSTY_SEARCH_INDEX` (default `main`).

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/health` | GET | Liveness |
| `/indexes` | GET | List registered indexes |
| `/indexes/{id}/status` | GET | Chunk count + root path |
| `/indexes/{id}/search` | POST | Hybrid BM25 + vector search |

**REQUIRED dependency (#590).** trusty-search is gated by the required-context
preflight (`pipeline::context_gate`, §1.1): if `/health` is unreachable/unhealthy
and `require_search=true` (default), the review is **skipped** (not silently
approved) — `ReviewResult.status = Skipped`, an actionable error is set, no LLM
call is made, and the review is never posted. With `require_search=false` the run
proceeds **DEGRADED** and is loudly labelled non-authoritative.

### 6.3 trusty-analyze client (`integrations/analyze_client.rs`)

`HttpAnalyzeClient` over `PR_INTELLIGENCE_ANALYZER_URL` (default `http://localhost:7879`).

| Method | Endpoint | Timeout | Notes |
|--------|----------|---------|-------|
| `health()` | `GET /health` | 5s | Liveness |
| `is_available()` | `GET /health` | 5s | Cached 60s TTL |
| `has_analysis()` | `GET /health` + `GET /indexes` | 5s each | **Two-step cheap probe only — never `/quality`** |
| `complexity_hotspots()` | `GET /indexes/{id}/complexity_hotspots` | 180s | |
| `smells()` | `GET /indexes/{id}/smells` | 180s | |

**REQUIRED dependency (#590).** As of #590 trusty-analyze is no longer optional:
the required-context preflight (`pipeline::context_gate`, §1.1) treats a failed
`has_analysis()` two-step probe (or an absent analyze client) as "analyze
unavailable". When `require_analyze=true` (default) that **skips** the review
(`status = Skipped`, actionable error, never posted); when `require_analyze=false`
the run proceeds **DEGRADED** and is loudly labelled non-authoritative.

*Within* a review that has already passed the gate, the per-call enrichment
fetches (`complexity_hotspots()`, `smells()`) still fail-soft to empty results so
a transient per-query error degrades the context partially rather than re-deciding
the hard require/skip policy.

### 6.4 JIRA client (designed, not in v0.1)

Ticket ID extraction: `\b([A-Z][A-Z0-9]+-\d+)\b` from PR title + description.

Fallback chain: (1) JIRA REST `GET /rest/api/3/issue/{id}` if `ATLASSIAN_*` configured, (2) keyword vector search against knowledge index, (3) semantic search on PR title+description. All fail-open.

Env vars: `ATLASSIAN_URL`, `ATLASSIAN_EMAIL`, `ATLASSIAN_API_TOKEN`.

### 6.5 Slack client (designed, not in v0.1)

Two notification types:
- `notify_pr_review_complete()` — single-line mrkdwn: dry/live label, linked PR, verdict, findings count.
- `notify_pr_review_service_failure()` — Block Kit warning for required-service unavailability.

Dispatch precedence: `SLACK_NOTIFICATION_CHANNEL` + `SLACK_BOT_TOKEN` (`chat.postMessage`), then `SLACK_NOTIFICATION_WEBHOOK`. Never raises (Slack outage cannot break a review).

---

## 7. Configuration (`src/config/`)

### 7.1 Global config (`ReviewConfig`)

Resolution precedence: built-in defaults → TOML config file → environment variables → CLI flags.

Config file location: `--config <path>` flag or `$XDG_CONFIG_HOME/trusty-review/config.toml` (via `dirs` crate). Absence of a config file is not an error.

**PR Intelligence settings:**

| Env var | Default | Effect |
|---------|---------|--------|
| `PR_INTELLIGENCE_DRY_RUN` | `true` | `true` = log only; never post to GitHub |
| `PR_INTELLIGENCE_ENABLED_REPOS` | `*` | Comma-sep allow-list; `*` = all; `org/*` = org wildcard |
| `PR_INTELLIGENCE_EXCLUDED_REPOS` | `""` | Comma-sep deny-list; always overrides allow-list |
| `PR_INTELLIGENCE_EXCLUDED_AUTHORS` | `""` | Bot logins to skip |
| `PR_REVIEW_BOT_USERNAME` | `""` | App login; triggers manual/live when requested as reviewer |
| `PR_INTELLIGENCE_LOG_DIR` | `/data/app/data/pr-reviews` | Review log + dedup store directory |
| `PR_INTELLIGENCE_ANALYZER_URL` | `http://localhost:7879` | trusty-analyze URL |
| `PR_INTELLIGENCE_MIN_FINDINGS_TO_POST` | `1` | Global min-findings gate |
| `PR_INTELLIGENCE_SUPPRESS_ADVISORY_REVIEWS` | `false` | Global `suppress_advisory` |

**LLM / provider settings:**

| Env var | Default | Effect |
|---------|---------|--------|
| `TRUSTY_REVIEW_PROVIDER` | `bedrock` | Default provider (`bedrock` or `openrouter`) |
| `TRUSTY_REVIEW_REVIEWER_MODEL` | `us.anthropic.claude-sonnet-4-6` | Reviewer-role model |
| `TRUSTY_REVIEW_VERIFIER_MODEL` | `us.anthropic.claude-haiku-4-5-20251001-v1:0` | Verifier-role model (must be ACTIVE) |
| `TRUSTY_REVIEW_SUMMARIZER_MODEL` | (= verifier default) | Diff Stage-C model |
| `OPENROUTER_API_KEY` | `""` | OpenRouter auth |
| `AWS_REGION` | `us-east-1` | Bedrock region |
| `TRUSTY_SEARCH_URL` | `http://localhost:7878` | trusty-search endpoint |
| `TRUSTY_SEARCH_INDEX` | `main` | Index name |

**Required-context settings (#590):**

| Env var | Default | Effect |
|---------|---------|--------|
| `TRUSTY_REVIEW_REQUIRE_SEARCH` | `true` | When `true`, an unreachable trusty-search **skips** the review; `false` opts into a DEGRADED (non-authoritative) run |
| `TRUSTY_REVIEW_REQUIRE_ANALYZE` | `true` | When `true`, an unreachable/not-ready trusty-analyze **skips** the review; `false` opts into a DEGRADED run |

Truthiness: `false`/`0`/`no`/`off` relax; `true`/`1`/`yes`/`on` require;
unrecognised values are ignored (fail-closed — keep the stricter value).

**TOML config file model tables:**

```toml
[provider]
default = "bedrock"

[models.reviewer]
provider = "bedrock"
model = "us.anthropic.claude-sonnet-4-6"
temperature = 0.3

[models.verifier]
provider = "bedrock"
model = "us.anthropic.claude-haiku-4-5-20251001-v1:0"  # MUST be ACTIVE
temperature = 1.0

[models.summarizer]
provider = "openrouter"   # roles may mix providers
model = "anthropic/claude-haiku-4.5"
temperature = 0.0

# Required-context gate (#590). Both default to true; set to false ONLY to opt
# into an explicitly-labelled, non-authoritative DEGRADED review.
[context]
require_search = true
require_analyze = true
```

Bedrock model IDs in config are validated for the `us.` prefix at config-resolution time. Failure is surfaced at startup, not mid-review.

### 7.2 Per-repo config (`.github/code-intelligence.yml`)

Fetched from the reviewed repo's default branch via the GitHub Contents API. Fails open to defaults on any error.

| Key | Type | Default | Behavior |
|-----|------|---------|----------|
| `skip` | bool | `false` | Disable all reviews for this repo |
| `issue_threshold` | float | `0.75` | Min confidence to file tracker issue |
| `block_threshold` | float | `0.90` | Min confidence for BLOCK tier |
| `pr_threshold` | float | `0.95` | High-confidence flag threshold |
| `suppress` | list[str] | `[]` | Substring/word-overlap suppression patterns |
| `min_findings` | int | `1` | Min eligible findings to post |
| `suppress_advisory` | bool | `false` | Skip posting APPROVE/APPROVE* with no blocking findings |
| `file_issues_on_merge_only` | bool | `false` | Defer tracker issue creation until merge |
| `diff_filter.suppress_patterns` | list[str] | `[]` | Extra file patterns to drop from diff |
| `diff_filter.preserve_patterns` | list[str] | `[]` | Force-keep patterns (override built-in drops) |
| `diff_filter.ignore_whitespace` | bool | `true` | Drop whitespace-only hunks |
| `diff_filter.ignore_imports` | bool | `true` | Drop import-only hunks |
| `diff_filter.ignore_comments` | bool | `true` | Drop comment-only hunks |
| `diff_filter.fixture_summary_min_lines` | int | `30` | Fixture summarization threshold |

Validation: `issue_threshold`, `block_threshold`, `pr_threshold` must be in `[0.0, 1.0]` and `pr_threshold >= issue_threshold`. On violation, all three revert to defaults. Unknown keys are ignored.

**Suppression matching:** Case-insensitive. Match if pattern is a substring of `finding_desc` OR Jaccard word-set overlap ≥ `SUPPRESS_OVERLAP_THRESHOLD` (0.70). All suppression lookups fail-open.

---

## 8. HTTP API (`src/service/`)

Compiled only under the `http-server` feature (default ON).

| Route | Method | Purpose |
|-------|--------|---------|
| `/health` | GET | Liveness; version, dry-run flag, dependency reachability, configured role models |
| `/status` | GET | In-flight review count, last `verification_model_error` |
| `/review` | POST | Synchronous on-demand review (dry-run) |
| `/pr/github/webhook` | POST | GitHub PR webhook; HMAC-validated; event-filtered |

**Default port:** 7880 (distinct from trusty-search :7878 and trusty-analyze :7879).

**Webhook contract:**

| `pull_request` action | Behavior |
|-----------------------|----------|
| `review_requested` | Validate HMAC → classify trigger → dispatch pipeline async |
| `opened`, `synchronize`, `reopened` | 200 OK, no dispatch |
| `closed` | 200 OK, short-circuit |
| Other event types | 200 OK, ignored |

**Trigger classification:**

| Condition | trigger | Dry-run override |
|-----------|---------|-----------------|
| `requested_reviewer.login == PR_REVIEW_BOT_USERNAME` | `manual` | `force_live = true` |
| Login in `live_review_requesters` | `manual` | `force_live = true` |
| Any other `review_requested` | `auto` | `force_dry_run = true` |

`effective_dry_run = (config.dry_run OR force_dry_run) AND NOT force_live`

**Health response shape:**

```json
{
  "status": "ok",
  "version": "tr-0.1",
  "dry_run": true,
  "deps": {
    "trusty_search": { "required": true, "reachable": true },
    "trusty_analyze": { "required": false, "reachable": false },
    "llm": { "provider": "bedrock", "reviewer_model_ok": true, "verifier_model_ok": true }
  }
}
```

---

## 9. CLI surface (`src/main.rs`)

| Subcommand | Purpose | Key flags |
|------------|---------|-----------|
| `serve` | Start the webhook daemon | `--port` (default 7880), `--bind`, `--config` |
| `run <owner> <repo> <pr>` | One-shot review of a live GitHub PR | `--reviewer-model`, `--provider`, `--no-log` |
| `run --local-diff <path>` | Review a local unified diff file (no GitHub, always dry-run) | `--reviewer-model`, `--provider` |
| `compare <owner> <repo> <pr>` | Run same PR across multiple models; print comparison table | `--models <slug,...>`, `--provider`, `--local-diff` |
| `profile` | Generate longitudinal contributor profile | see §10 |

**Model flag conventions:**
- `--reviewer-model us.anthropic.claude-sonnet-4-6` — bare ID, uses default/selected provider
- `--reviewer-model bedrock/us.anthropic.claude-sonnet-4-6` — forces Bedrock
- `--reviewer-model openrouter/anthropic/claude-sonnet-4.5` — forces OpenRouter
- `--provider bedrock` or `--provider openrouter` — sets default for bare IDs

**Output:** Review body + verdict to stdout. Diagnostic logging to stderr (stdout must stay clean for piping). JSON + Markdown log written to `PR_INTELLIGENCE_LOG_DIR/{owner}/{repo}/{pr_number}/`.

**Push firewall applies to all CLI paths.** No CLI flag can enable git-write operations against reviewed repos.

---

## 10. Contributor profile pipeline (`src/profile/`)

**Purpose:** Longitudinal per-contributor code quality profiling. Aggregates commit history from a tga SQLite database into time-period batches, samples representative diffs, uses an LLM to identify recurring findings and write a narrative profile.

**Submodules:**

| Module | Responsibility |
|--------|----------------|
| `types/` | `ContributorProfile`, `PeriodBatch`, `SampledDiff`, `LongitudinalFinding`, `TokenCostSummary`, `Trajectory`, `TrendTag`, `PROFILE_VERSION` |
| `selector.rs` | `ContributorSelector`, `resolve_contributor()`, `resolve_db_path()` — identity resolution from tga DB |
| `batch.rs` | `assemble_period_batches()`, `Window` enum — period assembly |
| `diff_sampler/` | `DiffSamplerConfig`, `sample_diffs_for_batches()` — representative diff sampling |
| `batch_reviewer.rs` | `BatchReviewer` — per-period LLM review calls |
| `synthesizer.rs` | `Synthesizer` — longitudinal pattern synthesis across periods |
| `reporter.rs` | `Reporter` — profile output (JSON `profile.json` + Markdown `profile.md`) |
| `reporter_github.rs` | Optional GitHub issue creation per contributor |
| `error.rs` | `ProfileError` (thiserror) |

**CLI entry:** `trusty-review profile` (dispatched via `cli_profile::cmd_profile()`).

**Dependencies:** `tga` crate (database, identity resolution, `AuthorPeriodSummary`, diff_for_commit), `rusqlite` (direct SQL queries in selector/diff_sampler).

**Status:** Implemented in v0.1 (commits #576–#579). The verification pass in `batch_reviewer.rs` is a known TODO (tracked in #552 / #558 context).

**Personalization from profile:** Per-PR review personalization informed by the contributor's profile (#569) is designed but not yet implemented (blocked on #552 full pipeline).

---

## 11. Diff summarizer (designed, not in v0.1)

**Purpose:** Token-budgeted, noise-stripped, anti-regression-protected view of a PR diff. Prevents fixture/i18n churn from poisoning the review context budget (lesson L12).

Three-stage pipeline (deterministic-first, LLM-last):

**Stage A — FileFilter (deterministic, file-level):** Classifies each file as `KEPT`, `DROPPED`, or `SUMMARY_ONLY`. Preserve-first rules (security/, .env, secrets/, credential-filename substrings, credential-URL regex, user `preserve_patterns`) override all drop/summarize rules. Drops: lockfiles, snapshots, generated code, pure renames. Summary-only: fixture/i18n files exceeding `fixture_summary_min_lines` (default 30).

**Stage B — HunkFilter (deterministic, hunk-level):** Drops hunks from `KEPT` files by language rule (whitespace-only, import-only, comment-only — each config-gated). Purely deterministic; no LLM.

**Stage C — HunkClassifier (LLM, per-hunk):** Batches surviving hunks (`BATCH_SIZE` = 10 per call), calls the **summarizer-role** provider at temperature 0.0, requests `{hunk_id, classification, confidence, reason}` JSON array.

| Classification | Droppable |
|----------------|-----------|
| `substantive` | No |
| `mechanical` | Only if `confidence > DROP_CONFIDENCE_THRESHOLD` (0.7) |
| `uncertain` | No (kept) |

Fail-safe: ANY LLM error during a batch treats all hunks as `uncertain` (kept). Never drops a hunk because the classifier failed.

Output: `FilteredDiff.render_for_prompt(max_chars = 12,000)`. No manifest header injected into the prompt (framing-effect regression). Manifest is telemetry-only.

Noisy-file collapse also occurs independently at the diff-fetch stage in the pipeline (`NOISY_FILE_PATTERNS`, `NOISY_DIFF_MIN_LINES` = 30): `.tsv`, `.csv`, `nls/**/*.{properties,json}`, `messages/**`, `labels/**`, `fixtures/**/*.{json,xml,sql}`, `test*fixtures*/**`, `__generated__/**`, `generated/**/*.{java,ts,js}`.

Tracked in issue #551.

---

## 12. Persistence (`designed for v0.2`)

**Dedup claim store:** redb (workspace standard; preferred over SQLite for dep hygiene). Table keyed on `(owner, repo, pr_number, head_sha)`. Atomic insert-or-fail claim; stale claims (> `DEDUP_STALE_SECS` = 7200s) purged on each attempt; claim released in a `Drop`-equivalent guard. Cross-process safe for multi-worker server deployments.

**Review log:** Filesystem JSON + Markdown (current v0.1 implementation). `ReviewLog` trait planned so an alternative backend (redb, object storage) can be plugged later.

Tracked in issue #549.
