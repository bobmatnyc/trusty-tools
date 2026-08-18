Changed

- `ModelTier::Interaction` resolves `us.anthropic.claude-sonnet-4-6` on AWS
  Bedrock instead of returning `None`. The analysis tier stays unmapped there:
  its Opus 4.8 inference-profile id could not be verified, and the Sonnet
  profile's shape does not predict it.
