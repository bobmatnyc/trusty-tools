Fixed

- The per-finding verifier retries a transient failure instead of writing the
  finding off on the first error. The transport errors that made this pass fail
  on nearly every finding came from the round's own fan-out — 27 of 29 findings
  in one measured review returned `Transport` while a single call to the same
  model succeeded in ~845 ms — so fabrication detection was effectively off.
  Each finding now gets up to `[verification] max_attempts` calls (default 3)
  with exponential backoff and jitter, and the fan-out width is
  `[verification] concurrency` (default 4, unchanged) instead of a hardcoded
  constant. Both read `TRUSTY_REVIEW_VERIFY_MAX_ATTEMPTS` /
  `TRUSTY_REVIEW_VERIFY_CONCURRENCY` over the config file. See #4459.
- A finding the verifier still cannot reach after its last attempt is recorded
  `unverifiable` — never confirmed, never refuted — rather than `error_refuted`,
  which read as a judgment nothing made and clamped the finding's confidence to
  0.10. `ReviewResult` carries a new `unverified_count` so a consumer can see how
  much of a review went unchecked. A round that rendered no judgment on anything
  can no longer relax the verdict the model itself reported. See #4459.
