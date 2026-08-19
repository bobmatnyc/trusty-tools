Fixed

- **A synthesized narrative that rounds a measured figure is no longer dropped as
  fabricated**
  ([#6030](https://github.com/bobmatnyc/trusty-tools/issues/6030)).
  The numeric guardrail compared canonical strings, so an authorship summary
  writing a measured `top_author_share_pct` of 85.19 as "85%" produced
  `rejected (unverified figure) in authorship summary: 85` and the whole LLM
  narrative fell back to the deterministic composer. Live verification of #6037
  failed on exactly that.
- `allowed_numbers` now widens each measured figure with its conventional
  roundings — the truncation and the round-half-up form at every coarser decimal
  precision, carries included (85.19 admits 85.1, 85.2 and 85; 9.99 admits 10).
  Integer figures are not widened, so a measured 8234 still never admits 8,000,
  and a figure matching no measured value under any rounding is rejected exactly
  as before, dropping the field with its raw-response capture.
