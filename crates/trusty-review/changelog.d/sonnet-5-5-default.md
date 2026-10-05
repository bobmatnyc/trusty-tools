Changed
- The default reviewer model on Bedrock is now Claude Sonnet 5.5
  (`us.anthropic.claude-sonnet-5-5`), replacing Sonnet 4.6. The verifier and
  summarizer defaults stay Haiku 4.5. `--reviewer-model`,
  `TRUSTY_REVIEW_REVIEWER_MODEL` and the config file still override it.
- The default `compare` model set is now Haiku 4.5, Sonnet 4.6, Sonnet 5.5 and
  Opus 5.5, all on Bedrock `us.` profiles. Sonnet 4.5 is no longer in it.
