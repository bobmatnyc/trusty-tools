Fixed

`--provider <unparsable>` no longer swallows the `TRUSTY_REVIEW_PROVIDER` env
layer. `resolve_role` parsed `cli_provider.or(env_provider)`, and `Option::or`
falls through on absence only — so a CLI value that failed to parse still won
that slot, the env var was never consulted, and resolution dropped straight to
the config file or the built-in default. Each layer now parses on its own: a
present-but-unparsable value logs a warning and yields to the next layer,
keeping the documented CLI → env → config file → default chain intact.
