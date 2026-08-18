Changed

- `ModelTier::Interaction` resolves Claude Sonnet 4.6 instead of Opus 4.8, on both OpenRouter (`anthropic/claude-sonnet-4.6`) and the Anthropic first-party API (`claude-sonnet-4-6`). Bedrock's opus arms stay deliberately unmapped. See #5987.
- **Breaking:** `ModelTier::Haiku` is renamed to `ModelTier::Classification`, so the tier names its purpose rather than the model it currently resolves to. The mapped value is unchanged at Haiku 4.5 on every provider. See #5987.
