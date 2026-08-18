Changed

- The three roles ask for a model tier instead of naming a pinned id: reviewer →
  analysis tier, verifier and summarizer → haiku tier. On OpenRouter the
  reviewer now resolves Opus 4.8, so the model that does the judging is an opus
  model rather than the Bedrock Sonnet 4.6 id it fell back to before.
- The haiku roles resolve the same model as before —
  `us.anthropic.claude-haiku-4-5-20251001-v1:0` on Bedrock. What changed is that
  a date-stamped constant became a tier lookup, so a future haiku move is one
  edit in `trusty-common` rather than three here.
- Bedrock's opus tiers are unmapped, so a standalone Bedrock run keeps its
  Sonnet 4.6 reviewer default. The trusty-audit path configures OpenRouter and
  gets Opus 4.8.
- `--reviewer-model`, `TRUSTY_REVIEW_*_MODEL`, and the config file's
  `[models.*].model` are unaffected. The tier is the built-in layer, below all
  three, so an explicitly-named id still wins. The provider now resolves before
  the model so the tier has a provider to key on.
