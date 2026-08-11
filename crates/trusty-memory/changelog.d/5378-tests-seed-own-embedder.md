Fixed

- 64 tests no longer depend on a mock embedder another test happened to seed
  ([#5378](https://github.com/bobmatnyc/trusty-tools/pull/5378); same defect
  class as [#4413](https://github.com/bobmatnyc/trusty-tools/issues/4413)).
  `retrieval::shared_embedder()` is a process-wide `OnceCell`, so under
  `cargo test` — one process per binary — whichever sibling seeded it first
  satisfied every other test for free. Under nextest's process-per-test
  isolation each test got a virgin cell, reached for the real ONNX model, and
  failed on the HuggingFace download (HTTP 429 in CI), which is what reddened
  CI run 31438217228 shard 1. Every test that embeds now calls
  `seed_shared_embedder_with_mock()` itself — from the shared fixture where one
  exists (`tools::tests::test_state`/`test_state_warming`,
  `web::tests::test_state`, `messaging::tests::fresh_palace`,
  `prompt_context::tests::spin_up_test_daemon_with_palace`,
  `mcp_stdio_tools`'s `Fixture::new`/`seed_palace`) and inline in the eight
  tests that build `AppState` directly. CI reported 17 failures, but that was
  the subset that lost the HuggingFace lottery on one run rather than the
  population; measuring with the embedder made deterministically unavailable
  found 64. With the embedder unavailable, `cargo nextest run -p trusty-memory`
  goes from 655 passed / 64 failed to 719/719, so the suite no longer needs a
  model download at all.
