Added

- The pins a cold start records come from the latest published stable release of
  each tool, resolved once and written into `engagement.toml`. Every run after
  the first reads the file, so `latest` never reaches a second run and the
  engagement states the exact triple it ran
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A synthesised `engagement.toml` names its `[models]` table in full: OpenRouter
  as the provider, `anthropic/claude-opus-4.8` for the judging call, and
  `anthropic/claude-haiku-4.5` for the verifier and summarizer. Leaving the table
  out fell through to `trusty-review`'s own defaults, whose provider is Bedrock —
  an account this engagement never named. All four fields are written because the
  table is all-or-none, and because it sits above the built-in constants in
  `trusty-review`'s precedence chain, these are the models the audit actually runs
  on ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
