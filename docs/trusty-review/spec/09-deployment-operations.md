# 09 — Deployment & Operations

**Status:** DRAFT
**Part of:** [trusty-review spec](README.md)
**Cross-refs:** [01-architecture](01-architecture.md) · [04-llm-providers](04-llm-providers.md) · [05-integrations](05-integrations.md) · [08-interfaces](08-interfaces.md) · [10-lessons](10-lessons-and-rationale.md)
**Factual basis:** source-analysis §6 (adapters/probes), §9 (Slack), §12 (lessons); CLAUDE.md systemd/deploy patterns; workspace `trusty_common::launchd` (used by trusty-analyze `commands/service.rs`).

---

## 1. Two run modes (one binary)

**REV-800 — Mode parity.** The same `trusty-review` binary SHALL run as a one-shot CLI or a long-lived server, selected by subcommand (`run`/`compare`/`eval`/`list`/`stats` vs `serve`). (binding decision #4; doc 01 REV-010, doc 08)

| | Local / one-shot | Server / webhook |
|---|------------------|------------------|
| Entry | `trusty-review run …` | `trusty-review serve --port P` |
| Lifecycle | exits after one review | long-lived daemon |
| Trigger | CLI args | GitHub `review_requested` webhook |
| Dedup | in-process; `eval` sets skip_dedup | all 3 layers incl. redb cross-process |
| Posting | dry unless `--live`; `--local-diff` always dry | `effective_dry_run` (doc 06 REV-541) |
| Deps | trusty-search optional-degrade for `--local-diff`; required otherwise | trusty-search required; analyze optional |

---

## 2. Service installation (server mode)

**REV-801 — systemd unit (Linux production).** A systemd unit SHALL run `trusty-review serve`, modeled on the trusty-search unit in CLAUDE.md (Restart=on-failure, env file for secrets). Sketch:

```ini
[Unit]
Description=trusty-review PR Review Daemon
After=network.target trusty-search.service
Wants=network-online.target

[Service]
Type=simple
User=ec2-user
EnvironmentFile=/data/app/.env.trusty-review
ExecStart=/usr/local/bin/trusty-review serve --port 7880
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

**REV-802 — launchd (macOS dev).** On macOS the crate MAY reuse `trusty_common::launchd::LaunchdConfig` (as trusty-analyze's `commands/service.rs` does) to install/bootstrap a dev agent. (workspace grounding)

**REV-803 — Port.** Default server port SHOULD be distinct from the sibling daemons (`:7878` search, `:7879` analyze) — e.g. `:7880`. Configurable via `--port`. (consistency with workspace)

**REV-804 — Secrets.** GitHub App private key, webhook secret, OpenRouter key, AWS creds SHALL come from the environment/EnvironmentFile (never config-file plaintext in the repo). (source-analysis §10; workspace security posture)

---

## 3. Startup / readiness checks

**REV-805 — Dependency startup checks.** On `serve` start (and at the top of a CLI run), the binary SHALL probe (source-analysis §12.3, §12.10; doc 05):
1. **trusty-search** (`GET /health`) — REQUIRED. If unreachable at startup, log loudly; reviews will skip + Slack-notify until it returns (do not crash-loop the daemon).
2. **trusty-analyze** via the **two-step cheap probe** (`/health` + `/indexes`) — OPTIONAL; record reachability for `/health` output. NEVER probe `/quality`.
3. **LLM provider/model identity** per role — see REV-811.

  > **Rationale (lesson learned §12.3):** never use the O(corpus) `/quality` endpoint as a readiness probe.

**REV-811 — Verifier model startup probe (RECOMMENDED).** At startup, perform a cheap identity/liveness check of each configured role's model (doc 04 REV-341). If the **verifier** model is not ACTIVE/available, the daemon SHALL refuse to start in live mode (or start with a prominent `verification_model_error` alarm if dry-run), converting §12.1 into a startup failure rather than a silent all-approve.
  > **Rationale (lesson learned §12.1):** the worst production incident was a silently-inactive verifier model.

---

## 4. Observability & metrics

**REV-810 — Required alarms / events.** The service SHALL emit, at minimum (source-analysis §6.3, §12.1):

| Event / metric | Level / type | Trigger |
|----------------|--------------|---------|
| `verification_model_error` | ERROR + **alarm** | verifier LLM call fails with a config/lifecycle error class (doc 04 REV-340). **Must page/alert** — distinguishes model misconfig from transient. |
| `pr_review_service_unavailable` | ERROR + Slack | required dep (trusty-search / LLM) down (doc 05 REV-452). |
| `dedup_skip` | counter | a review skipped due to in-flight/claim/head-SHA dedup (doc 02 REV-101/REV-105). |
| `verdict_distribution` | counter by verdict | every completed review, tagged `APPROVE`/`APPROVE*`/`REQUEST_CHANGES`/`BLOCK`/`N/A`. |
| `LLMInputTokens` / `LLMOutputTokens` | gauge/counter | per LLM call, tagged `ReviewType=PRReview`, role (doc 04 REV-332). |
| `analyze_degraded` | counter | trusty-analyze unavailable → static analysis skipped (doc 05 REV-442). |

**REV-812 — Metric backend.** Backend is deployment-specific (CloudWatch in production per source-analysis §6.3; stderr-structured logs locally). The crate SHALL emit through a thin metrics abstraction so the backend is swappable. Logs to **stderr** only (source-analysis §11.2).

**REV-813 — Verdict-distribution monitoring (calibration).** Operators SHALL monitor `verdict_distribution`. A sudden spike to ~100% APPROVE is the *symptom* of the §12.1 failure mode; pairing this dashboard with the `verification_model_error` alarm gives two independent signals of the same regression.

---

## 5. Dry-run rollout discipline

**REV-820 — Dry-run default + staged rollout.** (source-analysis §12.8; doc 06 REV-540)
1. Deploy with `PR_INTELLIGENCE_DRY_RUN=true` (default). Pipeline runs fully; posts nothing; files calibration issues; logs locally.
2. Validate review quality against the calibration log / verdict distribution.
3. Opt-in repos via `PR_INTELLIGENCE_ENABLED_REPOS` while still dry, then flip `PR_INTELLIGENCE_DRY_RUN=false` per readiness.
4. The `review_requested`-only webhook gate (doc 08 REV-702) holds throughout, independent of dry-run.

  > **Rationale (lesson learned §12.8):** prevents premature live posting before quality is validated.

---

## 6. Operational runbook (sketch)

| Symptom | Likely cause | Action |
|---------|--------------|--------|
| Every PR is APPROVE; `verification_model_error` firing | inactive verifier model (§12.1) | Fix `TRUSTY_REVIEW_VERIFIER_MODEL` to an ACTIVE model; restart. |
| Reviews skipped + Slack "service unavailable" | trusty-search down or LLM auth/region wrong | Check `:7878 /health`; check AWS creds/region or `OPENROUTER_API_KEY`. |
| `analysis.available: false` but analyze is up | regression to O(corpus) probe (§12.3) | Verify `has_analysis()` uses `/health`+`/indexes`, not `/quality`. |
| Duplicate tracker/calibration issues | upsert broken (§12.7) | Verify tracker upsert searches by `code-review` label + title prefix. |
| Bedrock `ValidationException` on every call | missing `us.` prefix (§12.2) | Add the inference-profile prefix; config-time validation (doc 06 REV-552). |
| Duplicate concurrent reviews | dedup layer missing (§12.4) | Verify in-process + redb claim + `review_requested`-only gate. |
