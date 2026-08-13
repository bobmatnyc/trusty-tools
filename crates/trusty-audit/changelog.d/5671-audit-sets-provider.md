Fixed

- The `tga audit` child now carries `TRUSTY_REVIEW_PROVIDER=openrouter` and the
  reviewer, verifier and summarizer model ids alongside the engagement's
  `OPENROUTER_API_KEY`, so review inference actually reaches OpenRouter. Naming
  the key was never enough: `trusty-review` resolves `Provider::Bedrock` as the
  last precedence level for all three roles, so the key sat unread while the
  reviewer either failed on missing AWS credentials or silently billed Bedrock
  ([#5671](https://github.com/bobmatnyc/trusty-tools/issues/5671)).
- An optional `[models]` table in `engagement.toml` overrides the built-in
  OpenRouter slugs, so a model rename is a config edit rather than a release.
  Any of the four variables already set in the operator's environment is
  inherited rather than clobbered.
