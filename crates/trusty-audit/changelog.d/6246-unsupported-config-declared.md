Fixed
- An engagement config key this version does not act on is declared instead of
  discarded. The keys are captured at parse time, named in `package.toml` as
  `config_not_acted_on` (always emitted, so an empty array is a positive claim
  of full coverage), and stated in the return package's README and in the
  `excluded` lines a front end prints. Before this, a config requesting extra
  export families — per-commit tables, CODEOWNERS metadata, branch-protection
  metadata — produced output byte-identical to a run that requested none, with
  no skip or unsupported marker anywhere, so the operator was left inferring
  silence as "the config did nothing".
- The limit is stated rather than implied: this covers TOP-LEVEL keys. A stray
  key written after a table header belongs to that table in TOML and is still
  absorbed by that table's own type.
