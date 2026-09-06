Fixed

- The SM's per-call cost estimate reads `trusty_common::pricing` instead of the
  family-substring table in `core::sm::providers::pricing`. Two rates change:
  Opus is $5/$25 per MTok rather than $15/$75, and Haiku 4.5 is $1/$5 rather
  than $0.80/$4. Sonnet 5 prices at $2/$10 instead of falling into the Sonnet
  4.x arm at $3/$15. The OpenRouter-routed OpenAI ids carry over verbatim.
  `cost_per_million` and `estimate_cost_usd` keep their signatures, so every
  provider call site is unchanged; an unpriced model still estimates zero, now
  reported once through `warn_unknown_model_once` instead of silently. (#6875)
