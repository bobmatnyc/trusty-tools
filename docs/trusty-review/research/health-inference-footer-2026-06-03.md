# trusty-review 0.3.x — Health, Inference, and Footer Features

**Date:** 2026-06-03  
**Issues:** #728, #722, #719  
**Versions:** 0.3.0, 0.3.1, 0.3.2

---

## Overview

Three refinements to trusty-review's health reporting and review output completed in June 2026:

1. **Inference reachability probe** (0.3.0, #719) — proactive detection of Bedrock/OpenRouter availability.
2. **Required-dependency gating** (0.3.1, #722) — health status reflects only critical dependencies (trusty-search).
3. **Review comment footer** (0.3.2, #728) — model + token-usage transparency on posted PR reviews.

---

## Feature Details

### 0.3.0: Inference Reachability Probe (#719)

**Problem:** No visibility into LLM inference availability until a review is attempted.

**Solution:** `review_health` endpoint now includes an `inference` field:

- `ok` — AWS Bedrock and/or OpenRouter accessible.
- `unreachable` — both providers unreachable (network/DNS error).
- `auth_error` — provider reachable but authentication failed.
- `unknown` — probe could not determine status.

**Implementation:**
- Always-on cached probe: 10s TTL, 3s timeout.
- Covers both Bedrock and OpenRouter endpoints.
- Service status transitions to `degraded` when inference != `ok`.

**Consumer impact:** Operators and users get early warning of LLM unavailability before paying for a review run that will fail.

---

### 0.3.1: Required-Dependency Health Gating (#722)

**Problem:** Health status was ambiguous about which deps were critical vs. optional.

**Solution:** Centralized `compute_status` logic that:

- Reports `status: degraded` **only** when a **required** dependency (trusty-search) is unreachable.
- Non-required dependencies (trusty-analyze) being down does **not** degrade status.
- Applies consistently to HTTP `/health` and MCP `review_health` tool paths.

**Background:** As of #590, trusty-search is mandatory for contextual reviews; trusty-analyze is strongly preferred but can be opted out via config. The health response now reflects this distinction.

**Consumer impact:** Operators can see at a glance that trusty-search is the critical blocker; analyze unavailability is a non-event.

---

### 0.3.2: Review Comment Footer (#728)

**Problem:** Posted PR reviews had no source attribution or cost transparency.

**Solution:** Every posted review comment now ends with:

```
🤖 Reviewed by us.anthropic.claude-sonnet-4-6 · tokens ↑1234 ↓567 · est. $0.01
```

- **Model:** Reviewer model slug (e.g., `us.anthropic.claude-sonnet-4-6` for Bedrock Sonnet).
- **↑ input tokens:** LLM prompt token count.
- **↓ output tokens:** LLM completion token count.
- **Cost:** Estimated USD spend (sourced from Bedrock pricing tables or OpenRouter).

**Implementation:**
- Single-source-of-truth with `ReviewResult` struct fields: `model`, `input_tokens`, `output_tokens`, `cost_estimate_usd`.
- Generated in `pipeline/post.rs::finalize_review()`.
- Appears identically in dry-run output and live posts.

**Consumer impact:** Full transparency on model choice, token usage, and inference costs. Useful for cost allocation and model selection tuning.

---

## Health Response Schema (0.3.2+)

```json
{
  "status": "ok|degraded|unknown",
  "version": "0.3.2",
  "dry_run": true,
  "reviewer_model": "us.anthropic.claude-sonnet-4-6",
  "inference": "ok|unreachable|auth_error|unknown",
  "deps": {
    "trusty_search": {
      "required": true,
      "reachable": true
    },
    "trusty_analyze": {
      "required": false,
      "reachable": true
    }
  }
}
```

---

## Testing & Validation

All three features are exercised by:
- MCP tool `review_health` (probes health endpoint).
- HTTP `GET /health` (direct daemon endpoint).
- Integration tests in `service/handlers_tests.rs`.
- Manual dry-run reviews (footer visible in comment body).

---

## Future Directions

- Expand `inference` probe to include latency metrics (p50/p95 response time).
- Per-model cost breakdown (if review uses multiple reviewer candidates in future compare logic).
- Webhook signature validation before health-check calls (currently omitted for liveness simplicity).
