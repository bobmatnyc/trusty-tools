# 0002. Model-agnostic credential routing

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Crate `open-mpm`
- **Supersedes / Superseded by:** —

## Context

open-mpm's product differentiator is **model-agnostic dispatch**: any agent role
must be backable by a different model/backend, assignable per-agent with a
two-line TOML change and no code modifications (spec PRD FR-1.x;
`docs/open-mpm/spec/ARCHITECTURE.md` §5). Three credential sources coexist:

- a local-dev `claude` CLI OAuth token (`CLAUDE_CODE_OAUTH_TOKEN`), which **401s
  against the Anthropic REST API** and therefore must drive the CLI subprocess;
- a paid direct Anthropic key (`ANTHROPIC_API_KEY`) for lower-latency REST; and
- an OpenRouter key (`OPENROUTER_API_KEY`) as the deployment/CI fallback giving
  access to 500+ models.

Without a deterministic priority and a clean separation of "which credential"
from "which transport," dispatch becomes ambiguous and agents silently route to
the wrong backend.

## Decision

We will route every agent invocation through a **deterministic credential
priority** implemented in `src/llm/credentials.rs` (`pick_credentials()`):

| Priority | Variant | Selected when |
|---|---|---|
| 1 (highest) | `ClaudeCode` | the agent TOML sets `runner = "claude-code"` **and** `CLAUDE_CODE_OAUTH_TOKEN` is set → routes to `ClaudeCodeAgentRunner` (spawns the `claude` CLI). |
| 2 | `AnthropicDirect` | the agent TOML sets `[llm] use_anthropic_direct = true` **and** `ANTHROPIC_API_KEY` is set → POSTs `api.anthropic.com`. |
| 3 | `OpenRouter` | otherwise → routes through `openrouter.ai/api/v1`. |

Credential choice is **per-agent**, driven by a two-line TOML change (`model`,
`runner`, and optionally `use_anthropic_direct`). Beneath credentials sit the
**three runner kinds** (`RunnerKind::ClaudeCode` CLI subprocess,
`InProcess` fast path for read-only agents, and `Subprocess` for isolated
file/shell agents over NDJSON IPC), which in turn resolve to the concrete LLM
backend (Bedrock / AnthropicNative / OpenRouter-raw / async-openai). A
session-level model override takes precedence over both the TOML `model` and the
env-var credential.

## Consequences

- **Positive:** any role can switch models/backends with a TOML edit; the OAuth
  vs. REST distinction is handled correctly (OAuth tokens never hit the REST API).
- **Positive:** credential selection, runner selection, and transport are cleanly
  layered, so each can evolve independently.
- **Known correctness gap (#408):** one legacy CTRL dispatch path bypasses
  `pick_credentials()` and always routes via OpenRouter regardless of the
  configured override/credential; the ratatui `run_pm_task_with_history` path
  routes correctly. This is tracked as open issue
  [#408](https://github.com/bobmatnyc/trusty-tools/issues/408) and is the reason
  spec FR-1.4 is marked *partial*.
- **Neutral:** model-name qualification (prefixing `anthropic/` for bare Claude
  IDs) applies **only** when routing via OpenRouter; native/CLI paths use the raw
  model id.
