Fixed

- A dry run no longer blocks a live review of the same head for up to 2h.
  `run` without `--live` no longer opens the dedup store, and a review that
  cannot post never claims its head, so a later live `run` or
  `webhook-listen` review of that head no longer fails with "another review
  holds the in-progress dedup claim" (#9348).
- A live post whose request never reached GitHub now releases its dedup
  claim, so a retry can run at once. That covers a token that could not be
  resolved and a connection that failed — DNS failure, connection refused,
  connect timeout, as on an offline or off-VPN machine. A post GitHub
  rejected with a 4xx releases the claim too, so each bounded
  `webhook-listen` drain retry of such a delivery runs the reviewer again. A
  post that may have created the comment keeps the claim until the 7200s
  stale window ends, so the retry cannot post a second comment (#9348).
- A dry run of a head that a live review already completed now runs the
  review instead of reporting "skipped: duplicate of a completed review".
  That includes `run` without `--live` and a dry `webhook-listen` delivery: a
  requested reviewer other than the bot or a `live_review_requesters` login,
  or no reviewer login with `PR_INTELLIGENCE_DRY_RUN=true`. It
  still posts nothing, and a live re-run of that head is still skipped
  (#9348).
