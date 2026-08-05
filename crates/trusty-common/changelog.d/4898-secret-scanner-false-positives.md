Fixed

- memory secret-scanner no longer rejects ordinary prose, branch names, or short tokens as credentials (closes [#4898](https://github.com/bobmatnyc/trusty-tools/issues/4898))
  - a `+`-joined English phrase (`PM+instructions+subagents`) is recognised as prose instead of base64; `+` no longer disqualifies a token outright, and each segment must be character-class-uniform so encoder output like `j1u7nJd+tvZers+wdZyr` stays flagged
  - a delimiter segment may carry one capital anywhere, not only in first position, so a branch name like `fix-3696-slice1-gapA-emit` is a human identifier again
  - the 20-character length floor now runs before the credential-prefix test, so a 4-character token (`Asia`) can no longer match `AKIA`/`ASIA`; AWS key ids are matched by the all-uppercase shape of their first 20 bytes, which keeps `AKIAIOSFODNN7EXAMPLE-old` and a key-id/secret-key pair flagged while letting `ASIA-PACIFIC-ROLLOUT-NOTES` through
  - added a deterministic generated-encoder corpus as a ratchet on base64 and base64url miss rates
