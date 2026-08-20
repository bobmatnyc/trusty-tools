Fixed

- A model id whose shape names one provider no longer runs on another. An
  unprefixed OpenRouter slug (`anthropic/claude-opus-4.8`) used to take whatever
  `provider` the path defaulted to; on the #6093 mitigation run that was
  Bedrock, which rejected the id, and the render that completed used Bedrock's
  `us.anthropic.claude-sonnet-4-6` default instead. `resolve_provider_and_model`
  now routes an unambiguous shape to its own provider — slash-form slugs to
  OpenRouter, `us.anthropic.*` / `anthropic.*` ids to Bedrock,
  `accounts/fireworks/models/*` to Fireworks — and returns
  `LlmError::Validation` naming both the id and the provider when the two cannot
  be reconciled. A genuinely ambiguous bare id still uses the configured
  default, and an explicit routing prefix is overruled only by a shape that
  belongs to exactly one provider's catalogue — never by the dotted
  `vendor.model` guess, so `openrouter/anthropic.claude-x` keeps working.
  `resolve_provider_and_model` is now fallible; `build_provider`
  propagates the error, and `trusty-review report` raises it in its preflight,
  before a sweep spends minutes on a render that would use the wrong model.
