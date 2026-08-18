Added

- The pins a cold start records come from the latest published stable release of
  each tool, resolved once and written into `engagement.toml`. Every run after
  the first reads the file, so `latest` never reaches a second run and the
  engagement states the exact triple it ran
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A synthesised `engagement.toml` names its `[models]` table in full, selecting
  OpenRouter as the provider. Leaving it out fell through to `trusty-review`'s
  own default, which is Bedrock — an account this engagement never named. All
  four fields are written because the table is all-or-none, and the values are
  the crate's own verified slugs rather than new ones
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
