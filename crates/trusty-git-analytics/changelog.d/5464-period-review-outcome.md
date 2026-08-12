Fixed

- A provider failure during a period review no longer reads as a clean period. `PeriodReviewer::review_period` returns a `PeriodReview` — the findings plus a `skipped` field carrying the provider error — instead of a bare `Vec<LongitudinalFinding>` whose only failure signal was a `warn!` log. Zero findings and an unreachable provider were the same value, so a Bedrock outage across twelve quarters would have rendered as twelve clean quarters, and the trajectory derived from that would be a trend over a silently smaller sample.

  The shape was inherited from trusty-review's original `review_period`, and it is fixed here rather than after #5465 wires the first real caller to it. `PeriodReview` is deliberately not a `Result`: no caller can `?` one period's outage into aborting the whole run, which is the property the audit depends on. It is `#[non_exhaustive]`, so the parse-failure case can join it once `ChatRequest` can enforce a schema.

- `PeriodReviewer::from_slug_with_store` resolves a slug against an explicit `KeyStore`; `from_slug` is now that call with `default_store()`. `default_store` reads the machine's real keychain, so credential resolution had no deterministic test — both arms are covered now, matching the injection `trusty_common`'s own `provider_for` tests use.
