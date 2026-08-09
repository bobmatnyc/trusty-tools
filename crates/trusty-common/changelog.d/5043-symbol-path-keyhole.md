Fixed

- Secret detector no longer flags `::`-joined Rust symbol paths (closes [#5043](https://github.com/bobmatnyc/trusty-tools/issues/5043))
  - `Bm25Index::queryTopK`, `Sha256Hasher::finalizeInto`, `OAuth2Client::refreshToken` and `Utf8Error::validUpTo` all reached the mixed-case branch and blocked memory writes; `check_secret` runs even under `force`, so only `allow_secret_like` got past it
  - Root cause is the CamelCase rule, not the delimiter set: a segment with two capitals fails `is_human_word_segment` however the token is split, so adding `:` to `IDENTIFIER_DELIMITERS` — the fix the issue proposed — changes nothing
  - `is_symbol_path` decomposes on `::` and decides each segment on its CamelCase word structure: at most one digit run and one stray single letter per word, and a longest word of five letters — three when the segment is 8 bytes or shorter, a graduated floor rather than an exemption
  - The relaxation is keyed on `::` because it appears in no encoder alphabet; `-` and `_` are base64url's own symbols and `.` is the JWT separator, so relaxing the case rule for those measurably widens base64url misses on tokens with no colon at all, and still does not fix this issue
  - Generated-encoder ceilings are unmoved; the measured price is credentials a human writes in path syntax (`secretKey::<blob>`, 1017 → 2022 misses per 30k), pinned as a ratchet alongside a second ratchet for `::`-chunked blobs
- Added a consolidated recurrence corpus covering all seven cycles (#1667, #1676, #2800/#4216, #4312, #4739, #4898, #5043) — 26 false positives and 22 credential shapes in two tables, walked by two tests, so the next change sees the whole accumulated obligation instead of six scattered batteries
