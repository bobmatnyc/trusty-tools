Fixed

- Inline PR comments now lead with a verification caveat when the finding's own
  `verified` outcome owes one, so a refuted finding no longer reads exactly like
  a surviving one (#5312). A clean `refuted` says it was disproved and is not a
  merge blocker; `error_refuted` / `truncation_refuted` say the verifier was
  never reached and the claim is unverified, not disproved.
