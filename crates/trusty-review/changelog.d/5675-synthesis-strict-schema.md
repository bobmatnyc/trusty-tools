Fixed

- Report synthesis no longer fails on OpenRouter backends that enforce OpenAI
  strict schemas
  ([#5675](https://github.com/bobmatnyc/trusty-tools/issues/5675)). The
  `report_synthesis` schema was built by hand and never passed through
  `enforce_strict_mode`, so `findings.items` carried no
  `additionalProperties: false` and the provider rejected the call with
  `In context=('properties','findings','items'), 'additionalProperties' is
  required to be supplied and to be false`. The `repo_investigation` schema had
  the same gap and is fixed with it.
- Every response schema is now built through `ResponseSchema::new`, which
  applies `enforce_strict_mode` on construction, so a schema added later is
  strict-compliant without having to remember the call.
- Fields that strict mode makes required now state that an empty string is
  acceptable, so the model is not pushed to invent filler for a field it has
  nothing grounded to say in: `evidence`, `component`, `business_impact` and
  `cost_effort` in `report_synthesis`, and `business_impact` and `cost_effort`
  in `repo_investigation`. The numeric guardrail only rejects invented FIGURES,
  so a fabricated qualitative sentence would have reached the report unchecked.
- `repo_investigation`'s `line` is nullable rather than a bare integer, so a
  finding whose line cannot be placed says `null` instead of guessing.
