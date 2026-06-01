# 06 — Configuration

**Status:** DRAFT
**Part of:** [trusty-review spec](README.md)
**Cross-refs:** [02-pipeline](02-pr-review-pipeline.md) · [04-llm-providers](04-llm-providers.md) · [05-integrations](05-integrations.md) · [10-lessons](10-lessons-and-rationale.md)
**Factual basis:** source-analysis §8 (per-repo config), §10 (env vars), §11.2 (no global state), §12 (lessons).

Two configuration layers: (1) **global** service config (config file + env), and (2) **per-repo** `.github/code-intelligence.yml` fetched from each reviewed repo.

---

## 1. Global configuration model

**REV-500 — Config sources & precedence.** trusty-review SHALL resolve config from, in increasing precedence: built-in defaults → config file (TOML; `toml` is a workspace dep) → environment variables → CLI flags. The resolved `ReviewConfig` is an owned value passed into the pipeline (no global state). (source-analysis §11.2)

**REV-501 — Config file location.** A `--config <path>` flag and a default path (e.g. `$XDG_CONFIG_HOME/trusty-review/config.toml`, resolved via the `dirs` crate). Absence of a config file is not an error; env + defaults suffice. (consistency with workspace daemons)

### 1.1 Env-var mapping (today → trusty-review)

The Rust service SHALL read the same env-var names as the Python service so existing deployments need no relabeling. (source-analysis §10)

#### PR Intelligence settings (source-analysis §10.1)

| Env Var | Default | Effect |
|---------|---------|--------|
| `PR_INTELLIGENCE_DRY_RUN` | `true` | `true` = log only; `false` = post to GitHub. **Default ON.** |
| `PR_INTELLIGENCE_ENABLED_REPOS` | `*` | Comma-sep allow-list; `*` = all; `org/*` = org wildcard |
| `PR_INTELLIGENCE_EXCLUDED_REPOS` | `""` | Comma-sep deny-list; **always overrides** allow-list |
| `PR_INTELLIGENCE_EXCLUDED_AUTHORS` | `""` | Bot logins to skip (e.g. `duetto-snyk-svc`) |
| `PR_REVIEW_BOT_USERNAME` | `""` | App login; triggers manual/live when requested as reviewer |
| `PR_INTELLIGENCE_LOG_DIR` | `/data/app/data/pr-reviews` | Review-log + dedup-store directory |
| `PR_INTELLIGENCE_ANALYZER_URL` | `http://localhost:7879` | trusty-analyze sidecar URL |
| `PR_INTELLIGENCE_EVAL_MODEL` | (varies) | Secondary model for `eval`/`compare` (dry-run only) |
| `PR_INTELLIGENCE_MIN_FINDINGS_TO_POST` | `1` | Global min-findings gate (per-repo overrideable) |
| `PR_INTELLIGENCE_SUPPRESS_ADVISORY_REVIEWS` | `false` | Global `suppress_advisory` |
| `PR_INTELLIGENCE_FILE_ISSUES_ON_MERGE_ONLY` | `false` | Global `file_issues_on_merge_only` |

#### GitHub App settings (source-analysis §10.2)

| Env Var | Effect |
|---------|--------|
| `GITHUB_APP_ID` | Numeric app ID |
| `GITHUB_APP_CLIENT_ID` | App OAuth client ID |
| `GITHUB_APP_PRIVATE_KEY` | RSA PEM (newlines `\n`) |
| `GITHUB_INSTALLATION_ID_DUETTORESEARCH` | duettoresearch installation |
| `GITHUB_INSTALLATION_ID_HOTSTATS` | hotstats installation |
| `GITHUB_WEBHOOK_SECRET` | HMAC secret for `X-Hub-Signature-256` |
| `GITHUB_TOKEN` | PAT fallback |

#### LLM, search, JIRA, Slack (source-analysis §10.3; doc 04)

| Env Var | Default | Effect |
|---------|---------|--------|
| `TRUSTY_REVIEW_PROVIDER` | `bedrock` | Default provider when a role does not specify one (`bedrock`\|`openrouter`) |
| `TRUSTY_REVIEW_REVIEWER_MODEL` | `us.anthropic.claude-sonnet-4-6` | reviewer-role model |
| `TRUSTY_REVIEW_VERIFIER_MODEL` | `us.anthropic.claude-haiku-4-5-20251001-v1:0` | verifier-role model (must be ACTIVE) |
| `TRUSTY_REVIEW_SUMMARIZER_MODEL` | (= verifier default) | diff Stage-C model |
| `BEDROCK_MODEL_ID` | `us.anthropic.claude-sonnet-4-6` | Back-compat alias for reviewer model when on Bedrock |
| `OPENROUTER_API_KEY` | `""` | OpenRouter auth (doc 04) |
| `AWS_REGION` | `us-west-2` | Bedrock region |
| `TRUSTY_SEARCH_URL` | `http://localhost:7878` | trusty-search endpoint (REQUIRED dep) |
| `TRUSTY_SEARCH_INDEX` | `main` | Index name (search + analyze) |
| `TRUSTY_SEARCH_APEX_INDEX` | `""` | Optional secondary APEX index (doc 05 REV-421 option B) |
| `ATLASSIAN_URL` / `ATLASSIAN_EMAIL` / `ATLASSIAN_API_TOKEN` | `""` | JIRA/Confluence |
| `SLACK_NOTIFICATION_CHANNEL` / `SLACK_BOT_TOKEN` | `""` | Preferred Slack dispatch |
| `SLACK_NOTIFICATION_WEBHOOK` | `""` | Slack fallback |

**REV-502 — Pipeline tuning constants** (`MAX_DIFF_CHARS`, `MAX_CONTEXT_FILES`, `MAX_ENRICHMENT_ROUNDS`, `FIX_ISSUE_MAX_PER_PR`, confidence thresholds, `DEDUP_STALE_SECS`, etc.) SHALL have the source-analysis defaults (doc 02 §3.4) and MAY be overridable via config file but NOT required as env vars.

---

## 2. Per-repo config: `.github/code-intelligence.yml`

Fetched from the reviewed repo's default branch (doc 02 REV-103). **Fails open** to defaults on any fetch/parse error. (source-analysis §8.1)

### 2.1 Full schema

**REV-510** — (source-analysis §8.1)

| Key | Type | Default | Behavior |
|-----|------|---------|----------|
| `skip` | bool | `false` | Disable all reviews for this repo |
| `issue_threshold` | float | `0.75` | Min confidence to file tracker issue |
| `block_threshold` | float | `0.90` | Min confidence for BLOCK tier |
| `pr_threshold` | float | `0.95` | Min confidence for high-confidence flag |
| `suppress` | list[str] | `[]` | Substring/word-overlap suppression patterns |
| `min_findings` | int | `1` | Min eligible findings to post |
| `suppress_advisory` | bool | `false` | Skip posting APPROVE/APPROVE\* with no required-change findings |
| `file_issues_on_merge_only` | bool | `false` | Defer tracker issue creation until merge |
| `diff_filter.suppress_patterns` | list[str] | `[]` | Extra file patterns to drop from diff (Stage A) |
| `diff_filter.preserve_patterns` | list[str] | `[]` | Force-keep patterns (override built-in drops) |
| `diff_filter.ignore_whitespace` | bool | `true` | Drop whitespace-only hunks (Stage B) |
| `diff_filter.ignore_imports` | bool | `true` | Drop import-only hunks (Stage B) |
| `diff_filter.ignore_comments` | bool | `true` | Drop comment-only hunks (Stage B) |
| `diff_filter.fixture_summary_min_lines` | int | `30` | Fixture summarization threshold (Stage A) |

### 2.2 Validation

**REV-511** — `issue_threshold`, `block_threshold`, `pr_threshold` MUST be in `[0.0, 1.0]`, and `pr_threshold >= issue_threshold`. On violation, **all three** revert to defaults. Unknown keys are ignored. Parse/fetch errors → full default config (**fail-open**). (source-analysis §8.1)
  > **Rationale (lesson learned §12.9):** suppression and config lookups must never block a review.

### 2.3 Annotated example

```yaml
# .github/code-intelligence.yml  (in the REVIEWED repo, not trusty-review)
skip: false
issue_threshold: 0.80      # stricter than the 0.75 default
block_threshold: 0.92
pr_threshold: 0.95         # must be >= issue_threshold
min_findings: 1
suppress_advisory: true    # don't post APPROVE/APPROVE* with no required changes
suppress:
  - "missing docstring"    # substring or >=70% word overlap → dropped
  - "consider adding a test"
diff_filter:
  ignore_whitespace: true
  ignore_imports: true
  ignore_comments: true
  fixture_summary_min_lines: 30
  preserve_patterns:
    - "src/security/**"     # always shown in full, even if it looks generated
  suppress_patterns:
    - "**/*.snap"           # never show snapshot files in the diff
```

---

## 3. Suppression matching

**REV-530 — Matching algorithm.** `is_finding_suppressed(finding_desc, patterns)` (source-analysis §8.2, §4.4):
- Case-insensitive.
- Match if EITHER: (a) pattern is a substring of `finding_desc`, OR (b) `texts_similar(finding_desc, pattern)` — normalized word sets, Jaccard overlap ≥ `SUPPRESS_OVERLAP_THRESHOLD` (0.70).
- (Note: source-analysis also references a separate `FINDING_SIMILARITY_THRESHOLD` = 0.6 used by a related helper; the suppression path uses 0.70. The implementer SHALL keep both constants distinct and documented.)

**REV-531 — Merged pattern sources.** The active pattern list = `bot-suppress`-labeled-closed-issue patterns (doc 05 REV-409) ∪ repo `suppress` list. (source-analysis §4.4)

**REV-532 — Fail-open.** Any error retrieving labels or repo config → treat as no suppression rules. (source-analysis §12.9)

---

## 4. Dry-run, enabled/excluded repos & authors

**REV-540 — Dry-run default ON.** `PR_INTELLIGENCE_DRY_RUN` defaults `true`. Flip to live only by setting it `false` OR by the per-run `--live` flag (doc 08). The webhook handler further gates dispatch to `review_requested` events regardless of dry-run (doc 08 REV-700).
  > **Rationale (lesson learned §12.8):** dry-run rollout discipline prevents posting un-validated reviews.

**REV-541 — Effective dry-run formula.** `effective_dry_run = (config.dry_run OR force_dry_run) AND NOT force_live`, where `force_live` / `force_dry_run` are determined by the trigger classification (doc 08 REV-701). (source-analysis §1.2)

**REV-542 — Repo gating.** Enabled = match `PR_INTELLIGENCE_ENABLED_REPOS` (`*`, `org/*`, or exact). Excluded = match `PR_INTELLIGENCE_EXCLUDED_REPOS` (always wins). Author exclusion = login in `PR_INTELLIGENCE_EXCLUDED_AUTHORS` → silently skip before dispatch. (source-analysis §1.2, §10.1)

---

## 5. Model-selection config (per-run + per-role)

**REV-550 — RoleModels in config.** The config file SHALL accept a `[models]` table mapping each role to `{ provider, model, temperature?, max_tokens? }`. (doc 04 REV-311/REV-323)

```toml
[provider]
default = "bedrock"          # used when a role omits `provider`

[models.reviewer]
provider = "bedrock"
model = "us.anthropic.claude-sonnet-4-6"   # us. prefix required for Bedrock
temperature = 0.3

[models.verifier]
provider = "bedrock"
model = "us.anthropic.claude-haiku-4-5-20251001-v1:0"   # MUST be foundation-lifecycle ACTIVE
temperature = 1.0

[models.summarizer]
provider = "openrouter"      # roles may mix providers
model = "anthropic/claude-haiku-4.5"        # OpenRouter slug — no us. prefix
temperature = 0.0
```

**REV-551 — Per-run override.** CLI flags `--provider`, `--reviewer-model`, `--verifier-model`, `--summarizer-model` (doc 08) override the `[models]` table for that run. `eval` accepts a list of reviewer models to compare. (doc 04 REV-312)

**REV-552 — Bedrock prefix validation at config time.** When a role's provider is `bedrock`, the model SHALL be validated for the `us.` prefix at config-resolution time (doc 04 REV-320), failing loudly before any review runs.
  > **Rationale (lesson learned §12.2):** surface inference-profile errors at startup, not mid-review.
