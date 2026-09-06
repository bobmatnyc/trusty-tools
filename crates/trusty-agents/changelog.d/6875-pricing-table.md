Fixed

- Cost figures no longer bill current-generation models at 4.x-generation
  rates. `perf::cost_usd` reads `trusty_common::pricing` instead of this crate's
  own table, which had no Sonnet 5, Opus 5 or Haiku 4.5 row and answered every
  unrecognised id with Sonnet-class rates — so a Sonnet 5 turn was priced at
  $3/$15 per MTok against a published $2/$10, a 50% overcharge on every REPL
  statusline, Costs tab and persisted daily total. An unpriced model now costs
  $0.00 and is reported once through `warn_unknown_model_once`, rather than
  producing a confident wrong number. Rates for the models the old table did
  price (Sonnet 4.5/4.6, Haiku 4, Opus 4.x) are unchanged. (#6875)
