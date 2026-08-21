Changed

- A model id whose shape contradicts the provider pinned for the call now
  resolves instead of failing (owner ruling 2026-08-21: "Models selection should
  be robust. We shouldn't fail on naming/id issue. The report should include
  which models are used."). Where the pinned provider publishes the same model,
  the id is translated into that provider's own **verified** spelling —
  `bedrock/anthropic/claude-sonnet-4.6` runs `us.anthropic.claude-sonnet-4-6`;
  where it does not, the call routes to the provider whose catalogue the id
  belongs to. The translation reads the verified `ModelTier` catalogue and never
  derives an id by string surgery, so an unmappable direction (Bedrock publishes
  no Opus 4.8 profile) resolves by routing rather than by inventing an id. The
  #6114 guarantee is unchanged in substance and moves from refusal to
  attribution: every adjustment is logged and printed in the report as
  `requested → ran`. `LlmError::Validation` remains for the one case with no
  reasonable resolution — an id belonging to a provider this build cannot call,
  with no verified equivalent in one it can. The MCP `review_pr` / `review_diff`
  `reviewer_model` override follows the same rule.
