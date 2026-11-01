Fixed
- `memory_note` and `memory_remember` no longer skip a `fact_key` slot rewrite as a near-duplicate. A new value that differs from the slot's occupant by a few characters, such as a step number, now replaces the occupant instead of leaving the stale value in recall. Writes without an admitted slot keep the 5-minute near-duplicate window (#10643).
- A malformed `expires_at` on either tool is now an error even when the content would also trip a content gate; it no longer returns a "skipped" success envelope (#10643).
