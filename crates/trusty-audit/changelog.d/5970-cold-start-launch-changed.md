Changed

- The built-in reviewer default is `anthropic/claude-opus-4.8`, was
  `anthropic/claude-sonnet-4.6`. This changes behavior for EXISTING configs, not
  only for newly written ones: an auditor-supplied `engagement.toml` with no
  `[models]` table resolves through this constant, so the judging call on the
  common handoff path moves to the Opus analysis tier. The verifier and
  summarizer stay on `anthropic/claude-haiku-4.5`, and a config that names its
  own `[models]` table is unaffected — it outranks the built-in
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
