Fixed
- `trusty-review webhook-listen` now drains its webhook inbox into the review pipeline instead of holding acknowledged deliveries forever. Dependencies build lazily on the first actionable delivery and open the dedup store as `Required`.
- The `review_requested` filter moved to `webhook_drain` and is shared with the legacy `POST /pr/github/webhook` route, so the two transports cannot drift on which deliveries cause a review (#5192).
