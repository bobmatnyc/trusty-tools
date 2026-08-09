Fixed
`trusty-review webhook-listen` now drains its webhook inbox into the review pipeline instead of holding acknowledged deliveries forever. Dependencies build lazily on the first actionable delivery and open the dedup store as `Required`. The `review_requested` filter moved to `webhook_drain` and is shared with the legacy `POST /pr/github/webhook` route (#5192).

Changed
**Breaking.** `webhook_listener::run` now takes a `ReviewConfig` — it needs one to build the pipeline. `ReviewDeps` additionally derives `Clone` (additive). Under Cargo's 0.x rule the signature change requires the MINOR position bumped at release (#5192).
