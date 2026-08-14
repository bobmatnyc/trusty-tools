Fixed

- **An unparsable `--provider` value no longer swallows the
  `TRUSTY_REVIEW_PROVIDER` env layer**
  ([#5679](https://github.com/bobmatnyc/trusty-tools/issues/5679)).
  `resolve_role` parsed `cli_provider.or(env_provider)`, and `Option::or` falls
  through on absence only — so a CLI value that failed to parse still won that
  slot, the env var was never consulted, and resolution dropped straight to the
  config file or the built-in default. Running `--provider garbage` with
  `TRUSTY_REVIEW_PROVIDER=openrouter` exported reached Bedrock, not OpenRouter.
- Each precedence layer now parses on its own through `parse_provider_layer`, so
  a present-but-unparsable value logs a warning naming its source and yields to
  the next layer, keeping the documented CLI → env → config file → default chain
  intact.
