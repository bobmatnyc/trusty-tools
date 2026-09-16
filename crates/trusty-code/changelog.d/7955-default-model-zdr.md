Fixed
- `DEFAULT_MODEL` now resolves to `anthropic/claude-sonnet-4.5` instead of
  `openai/gpt-4o-mini`, which OpenRouter accounts with the zero-data-retention
  (ZDR) guardrail reject with a `404 zdr-violation-by-guardrail`.
- A 404 from OpenRouter (or any OpenAI-compatible provider) on an inference
  call now surfaces an actionable error naming the requested model and
  pointing at the `--model`/`model_override` and `tcode config keys list`
  remedies, instead of a bare HTTP status.
