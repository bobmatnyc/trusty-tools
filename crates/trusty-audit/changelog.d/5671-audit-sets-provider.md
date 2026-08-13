Fixed

- The `tga audit` child now carries `TRUSTY_REVIEW_PROVIDER=openrouter` and the
  reviewer, verifier and summarizer model ids alongside the engagement's
  `OPENROUTER_API_KEY`, so review inference actually reaches OpenRouter. Naming
  the key was never enough: `trusty-review` resolves `Provider::Bedrock` as the
  last precedence level for all three roles, so the key sat unread while the
  reviewer either failed on missing AWS credentials or silently billed Bedrock
  ([#5671](https://github.com/bobmatnyc/trusty-tools/issues/5671)).
- The provider and the three model ids are resolved as one selection from a
  single layer — the operator's environment, else the engagement config, else
  the built-in slugs. A layer that names some of the four but not all four is
  refused before any child is spawned, naming what it set and what it left
  unset. Resolving them independently would pair an operator's
  `TRUSTY_REVIEW_PROVIDER=bedrock` with this crate's OpenRouter slugs, which is
  the same HTTP 400 reached from the other direction; nothing downstream catches
  that pairing ([#5679](https://github.com/bobmatnyc/trusty-tools/issues/5679)).
- An optional `[models]` table in `engagement.toml` overrides the built-in
  slugs, so a model rename is a config edit rather than a release. It rejects
  unknown keys, so `reviewr` is a parse error instead of a silent fallback to
  the default.
