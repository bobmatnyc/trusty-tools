# trusty-review — Architecture

> **Status:** Current (v0.1.0 · 2026-06-01)
> **Derived from:** spec docs 01, 09, 10 + source audit of `crates/trusty-review/src/`

---

## 1. System context

trusty-review is the third member of the `trusty-tools` PR-review triad. It orchestrates; it does not index or analyze on its own.

```
Triggers
  ├── GitHub webhook (review_requested)
  └── CLI one-shot (run / compare / profile)

trusty-review (crates/trusty-review/)
  ├── axum router           — service::build_router() [http-server feature]
  ├── Review pipeline       — pipeline::runner::run_review()
  │     ├── diff module     — load, truncate, identifier extraction
  │     ├── grade module    — severity-anchored deterministic grade derivation
  │     ├── prompt module   — reviewer system prompt + context assembly
  │     ├── parser module   — verdict + findings extraction from LLM response
  │     └── output module   — log file writing + stdout rendering
  ├── LLM layer             — llm::LlmProvider trait (Bedrock | OpenRouter)
  ├── Profile pipeline      — profile:: (longitudinal contributor profiling)
  └── Integrations          — GitHub client, search/analyze HTTP clients

Workspace dependencies (HTTP, required for full reviews)
  ├── trusty-search :7878   — REQUIRED (code context retrieval)
  └── trusty-analyze :7879  — OPTIONAL (static analysis; graceful degradation)

External services
  ├── AWS Bedrock           — Converse API (default provider)
  ├── OpenRouter            — fallback / alternative provider
  ├── GitHub REST/GraphQL   — PR diffs, webhooks, issue tracking
  ├── JIRA REST             — ticket context (optional; fail-open)
  └── Slack                 — notifications (optional; fail-open)
```

---

## 2. Crate layout

```
crates/trusty-review/
├── Cargo.toml              edition = "2024" (uses let-chains)
├── README.md
└── src/
    ├── lib.rs              re-exports; crate-level docs
    ├── main.rs             binary: clap dispatch → CLI or `serve`
    ├── cli_profile.rs      `profile` subcommand dispatch
    ├── config/
    │   ├── mod.rs          ReviewConfig, Provider enum; env+file resolution
    │   ├── constants.rs    confidence thresholds, pipeline tuning constants
    │   └── role_models.rs  RoleModels, RoleConfig, FileModels, RoleCliOverrides
    ├── llm/
    │   ├── mod.rs          LlmProvider trait, LlmRequest/Response, build_provider()
    │   ├── error.rs        LlmError (thiserror enum)
    │   ├── models.rs       model ID constants + COMPARE_CANDIDATE_MODELS
    │   ├── bedrock/        BedrockProvider (Converse API)
    │   │   ├── mod.rs
    │   │   ├── pricing.rs  per-model cost table
    │   │   ├── tool_use.rs structured-output (JSON tool-use) support
    │   │   └── tests.rs
    │   ├── openrouter.rs   OpenRouterProvider (wraps trusty_common::chat)
    │   └── openrouter_tests.rs
    ├── models/
    │   └── mod.rs          Verdict, Effort, VerifyOutcome, Finding, ReviewResult
    ├── pipeline/
    │   ├── mod.rs          re-exports; submodule map
    │   ├── diff.rs         DiffSource, diff loading, truncation, identifier extraction
    │   ├── grade.rs        derive_verdict() — severity-anchored grade derivation
    │   ├── prompt.rs       reviewer_system_prompt(), build_review_prompt()
    │   ├── parser.rs       parse_review_response() → ParsedReview
    │   ├── output.rs       write_review_log(), print_review_result(), log_json_path()
    │   ├── runner.rs       ReviewDeps, ReviewInput, run_review() — top-level orchestration
    │   ├── parser_tests.rs
    │   └── prompt_tests.rs
    ├── integrations/
    │   ├── mod.rs
    │   ├── search_client.rs  HttpSearchClient (trusty-search :7878)
    │   ├── analyze_client.rs HttpAnalyzeClient (trusty-analyze :7879)
    │   └── github/
    │       ├── mod.rs      GithubClient
    │       ├── auth.rs     GitHub App JWT + installation token + PAT fallback
    │       ├── pr.rs       fetch_pr_diff(), fetch_pr_files(), fetch_pr_meta()
    │       ├── firewall.rs assert_no_push_operation() — hard-coded push firewall
    │       └── webhook.rs  webhook signature verification
    ├── profile/
    │   ├── mod.rs          re-exports for longitudinal profiling
    │   ├── types/          ContributorProfile, PeriodBatch, SampledDiff, etc.
    │   ├── selector.rs     ContributorSelector, resolve_contributor(), resolve_db_path()
    │   ├── batch.rs        assemble_period_batches() (Window enum)
    │   ├── diff_sampler/   DiffSamplerConfig, sample_diffs_for_batches()
    │   ├── batch_reviewer.rs BatchReviewer — per-period LLM review calls
    │   ├── synthesizer.rs  Synthesizer — longitudinal pattern synthesis
    │   ├── reporter.rs     Reporter — profile output (JSON + Markdown)
    │   ├── reporter_github.rs optional GitHub issue creation
    │   └── error.rs        ProfileError (thiserror)
    └── service/            [http-server feature only]
        ├── mod.rs          build_router(), serve(), DEFAULT_PORT=7880
        ├── handlers.rs     AppState, handle_health(), handle_status(), handle_review()
        ├── handlers_tests.rs
        └── webhook.rs      handle_github_webhook() — HMAC-validated GitHub events
```

**Key invariants (enforced by code structure):**
- `axum` + `tower-http` are optional dependencies behind the `http-server` feature (default ON). Library consumers using `--no-default-features` can use the pipeline without the HTTP stack.
- `thiserror` error enums in library modules; `anyhow::Result` in `main.rs` binary.
- No `unwrap()` in library code; no global state; logs to stderr only.
- No source file exceeds 500 lines (workspace hard cap).
- `edition = "2024"` (let-chains used in pipeline dispatch).

---

## 3. Dependency topology

### 3.1 Required vs optional

| Dependency | Status | Behavior when absent |
|------------|--------|----------------------|
| **trusty-search** (:7878) | **REQUIRED** | Review is skipped; Slack service-failure notice sent |
| **AWS Bedrock** or **OpenRouter** | **REQUIRED** (one active) | Review is skipped; Slack service-failure notice sent |
| **trusty-analyze** (:7879) | **OPTIONAL** | Pipeline proceeds with empty static-analysis context |
| **JIRA REST** | optional; fail-open | Empty ticket context |
| **Slack** | optional; fail-open | Notifications silently dropped |
| **GitHub App auth** | optional; PAT fallback | PAT used if App not configured |

All optional dependency failures are fail-open and never block a review.

### 3.2 trusty-search and trusty-analyze: HTTP only

Both sibling daemons are consumed over HTTP. In-process linking is intentionally avoided:
- The daemons own heavyweight separately-managed runtimes (embedder pool, redb stores, GPU lifecycle).
- The HTTP boundary enables graceful daemon restart without taking down the review service.
- Both clients are behind a trait (`SearchClient`, `AnalyzeClient`) so the transport is swappable.

The `in-process-search` cargo feature is reserved for future single-binary CLI use but is not implemented in v0.1.

### 3.3 trusty-analyze readiness: two-step cheap probe

`has_analysis()` uses only `GET /health` (O(1)) + `GET /indexes` (O(n_indexes)).  
**Never** `GET /indexes/{id}/quality` — that endpoint is O(corpus) and always times out as a probe.  
This was the most consequential lesson from the Python predecessor (L3).

---

## 4. Run modes (one binary)

The same `trusty-review` binary runs in two modes selected by subcommand:

| Mode | Entry | Lifecycle | Trigger |
|------|-------|-----------|---------|
| **CLI one-shot** | `run`, `compare`, `profile` | exits after one review | CLI args |
| **Webhook daemon** | `serve --port 7880` | long-lived daemon | GitHub `review_requested` webhook |

Server mode differences:
- Full three-layer dedup (in-process + per-SHA + redb cross-process claim).
- `effective_dry_run` formula from trigger classification.
- axum router compiled under `http-server` feature.
- Graceful shutdown via `trusty_common::shutdown_signal()` (SIGTERM/SIGINT).

CLI one-shot differences:
- `run --local-diff <path>` reviews a local diff file: no GitHub credentials needed, always dry-run.
- `compare` runs the same PR across the COMPARE_CANDIDATE_MODELS set (default: Bedrock Haiku 4.5, Sonnet 4.5, Sonnet 4.6).
- `profile` runs the longitudinal contributor-profile pipeline against a tga SQLite database.

---

## 5. Grade derivation: severity-anchored floor

The pipeline's core quality innovation is a two-pass deterministic grade derivation in `pipeline/grade.rs`:

**Pass 1 — Low-confidence override (ceiling):** If ALL findings have `confidence ≤ 0.65` AND none are `High`-effort, force `APPROVE`. Prevents `APPROVE*` over-fire on clean PRs with speculative advisory noise.

**Pass 2 — Severity floor (minimum):** Take the stricter of (model-proposed, severity-derived floor):

| Finding set | Minimum floor |
|-------------|---------------|
| Any `High`-effort finding (critical/compile-break) | BLOCK |
| ≥2 `Medium`-effort findings | REQUEST_CHANGES |
| Exactly 1 `Medium`-effort finding | APPROVE* |
| Only `Low`-effort or no findings | APPROVE |

`Verdict::Unknown` is always preserved (diff was unassessable; not a clean APPROVE).

Calibration against 30 live PRs showed the model under-fires without this: 0% BLOCK detection, REQUEST_CHANGES only 36% of the time. The floor rules fix both regressions without changing the model.

---

## 6. Fail-safe posture

**Default verdict is APPROVE; the bot bears the burden of proof.** This is not just a review philosophy — it is encoded in the system prompt and enforced by the grade derivation:
- Pipeline errors fall back to `Verdict::Approve` (not UNKNOWN, not BLOCK).
- `Verdict::Unknown` is reserved for "model could not assess the diff" (diff too truncated).
- All optional dependency failures are fail-open (empty context, proceed).
- Suppression and per-repo config failures are fail-open.
- LLM transport errors during verification are treated as REFUTED (not CONFIRMED), failing toward APPROVE.

The one alarm that breaks the "always proceed" posture: a `verification_model_error` from a config/lifecycle LLM error. This fires at ERROR level and is designed to be loud — silent all-APPROVE from an inactive verifier model was the worst production incident in the Python predecessor (L1).

---

## 7. Deployment model

### 7.1 Server mode

```
[GitHub] --webhook--> [trusty-review serve :7880]
                             |
                    [trusty-search :7878]  (REQUIRED)
                    [trusty-analyze :7879] (OPTIONAL)
                    [AWS Bedrock / OpenRouter]
                    [redb dedup claim]
                    [filesystem review log]
                             |
                    [GitHub PR comment + tracker issue]
                    [Slack notification]
```

Default port 7880 (distinct from search :7878 and analyze :7879).  
Default `PR_INTELLIGENCE_DRY_RUN=true` — never posts to GitHub without explicit opt-in.

### 7.2 Systemd (Linux production)

```ini
[Unit]
Description=trusty-review PR Review Daemon
After=network.target trusty-search.service

[Service]
Type=simple
User=ec2-user
EnvironmentFile=/data/app/.env.trusty-review
ExecStart=/usr/local/bin/trusty-review serve --port 7880
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info
```

All secrets (GitHub App private key, webhook secret, AWS credentials, OpenRouter key) come from the `EnvironmentFile`; never config-file plaintext.

### 7.3 macOS dev

The crate MAY reuse `trusty_common::launchd::LaunchdConfig` (as trusty-analyze does) to bootstrap a dev launchd agent. Binary install via `cargo install --path crates/trusty-review --locked`.

### 7.4 Dry-run rollout discipline

1. Deploy with `PR_INTELLIGENCE_DRY_RUN=true` (default). Pipeline runs fully; posts nothing; files calibration issues; logs locally.
2. Validate review quality against the calibration log and verdict distribution.
3. Opt-in specific repos via `PR_INTELLIGENCE_ENABLED_REPOS` while still dry.
4. Flip `PR_INTELLIGENCE_DRY_RUN=false` only after quality is validated.

The `review_requested`-only webhook gate holds throughout, independent of dry-run.

---

## 8. Observability

| Event / metric | Level | Trigger |
|----------------|-------|---------|
| `verification_model_error` | ERROR + **alarm** | Verifier LLM call fails with config/lifecycle error. Must page — distinguishes model misconfig from transient. |
| `pr_review_service_unavailable` | ERROR + Slack | Required dep (trusty-search or LLM) is down. |
| `dedup_skip` | counter | Review skipped due to dedup (in-flight/claim/head-SHA). |
| `verdict_distribution` | counter by verdict | Every completed review. A spike to ~100% APPROVE is the symptom of an inactive verifier model (L1). |
| `LLMInputTokens` / `LLMOutputTokens` | gauge/counter | Per LLM call, tagged by role. |
| `analyze_degraded` | counter | trusty-analyze unavailable → empty static analysis. |

Metric backend is deployment-specific (CloudWatch in production; structured stderr logs locally). The crate emits through a thin abstraction so the backend is swappable.

---

## 9. Design rationale (lessons from the Python predecessor)

| Lesson | Binding design decision |
|--------|------------------------|
| **L1** Inactive verifier model → silent all-APPROVE | `verification_model_error` alarm; startup model probe; model IDs validated at config time |
| **L2** Bedrock inference-profile prefix required | `us.` prefix validated at config-resolution time; config-time failure, not mid-review |
| **L3** Analyzer readiness probe timed out | Two-step cheap probe only (`/health` + `/indexes`); never `/quality` |
| **L4** Concurrent webhook delivery → duplicate reviews | Three dedup layers: in-process + per-SHA + redb atomic claim |
| **L5** Verifier candidate selection bug | Verdict-conditioned selection: confidence ≥ 0.50 when REQUEST_CHANGES/BLOCK |
| **L6** Truncation shortcut over-refuted real findings | Full `diff_text` (bounded to `MAX_DIFF_CHARS`) as verification window |
| **L7** Calibration tracker created duplicates | Upsert-per-PR: one tracker issue per PR, updated in place |
| **L8** Premature live posting | Dry-run default ON; staged rollout discipline |
| **L9** Suppression blocked a review | All suppression and config lookups fail-open |
| **L10** trusty-analyze down blocked reviews | Optional dependency; degrades silently to empty context |
| **L11** Bot force-pushed to infra repo | Hard-coded non-configurable push firewall (`GH_ALLOW_PUSH = false`) |
| **L12** Fixture/i18n churn buried real changes | Noisy-file collapse + diff Stage A/B/C filtering |
| **L13** PR deleted a permission check; orphaned producer missed | Cross-reference blast-radius search in parallel context retrieval |
