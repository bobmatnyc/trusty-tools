Fixed

- Resolved a broken rustdoc intra-doc link in `grounding::search_rpc` (a bare
  `[`Display`]` now points at `std::fmt::Display`), unblocking the rustdoc
  intra-doc-link publish gate. No behavior change (#6285).
