Fixed

- `persist.rs` and `aggregator/mod.rs` cited a
  `report::tests::persist_weekly_engineer_upserts_rows` that did not exist, so
  the `agentic_pct` formula DOC-67 §8 reports had no test naming it. That test
  now exists and runs eight real commit shapes — a multi-trailer house footer, a
  bare Claude Code footer, a Copilot trailer, a forge squash-merge whose body was
  replaced, a machine merge summary, a hand-written commit, a revert and a
  bot-authored commit identified only by its email — through `ai_markers::detect`
  and the aggregator, then reads the persisted row back. The stale
  `persist_weekly_quality_upserts_rows` pointer is corrected too, and all four
  rows leave the test-pointer ratchet allowlist.
