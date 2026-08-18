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
