Fixed

- Report synthesis no longer hard-fails a due-diligence run when a provider
  ignores the forced JSON schema and returns markdown-headed free prose
  instead ([#6009](https://github.com/bobmatnyc/trusty-tools/issues/6009)).
  Live repro against `anthropic/claude-opus-4.8` via OpenRouter: `200 OK`,
  `finish_reason: "stop"`, `response_format` silently ignored, response was
  `## Executive Summary\n\n<prose>\n\n## Top Risks\n\n...`. The parser now
  recovers `executive_summary` from that shape (never `top_risks`/`findings`
  — prose is never reconstructed into structured rows); the numeric guardrail
  still verifies whatever text is recovered, so a fabricated figure is
  rejected exactly as before.
- An unparseable synthesis response is now persisted (scrubbed) next to the
  report output as `synthesis-unparseable-response.txt` so a future
  occurrence is diagnosable without spending another live provider call.
- Report synthesis no longer hard-fails a due-diligence run when a provider
  returns valid top-level JSON but drifts `top_risks` field names — a live
  capture used `risk` for `description` and `applications` for `apps`, and
  omitted `severity`/`cost` entirely. Those field names are now accepted as
  aliases, and an omitted `severity`/`cost` defaults to unset rather than
  failing the whole response; the rendered report shows `not stated in
  source data` for a defaulted severity/cost — never a fabricated band or
  figure.
- Fixed the CLASS of #6009, not just the two prior instances: three
  consecutive live calls to `anthropic/claude-opus-4.8` via OpenRouter, all
  with `response_format: json_schema`/`strict: true`, produced three
  different response shapes (markdown prose, `risk`/`applications`, and
  `risk`/`cost_effort_framing`/`affected_applications`) — `response_format`
  is best-effort for Anthropic-family models, since Anthropic's own API has
  no native strict-JSON mode. The synthesis system prompt now states the
  exact required JSON field names in the prompt TEXT itself, generated
  directly from the same schema definition the request forces via
  `response_format` (so the two can never drift apart), and the per-shape
  `#[serde(alias)]`s on `RiskRow` are replaced by a whitelist synonym table
  (`synthesize_normalize`) applied before typed deserialization: a
  recognised synonym (`risk`, `applications`, `affected_applications`,
  `cost_effort_framing`) renames onto its canonical field, and any other key
  is dropped with a recorded `synthesis: dropped unrecognized field` note —
  never guessed. The numeric guardrail still runs after normalization, so a
  fabricated figure is rejected exactly as before.
