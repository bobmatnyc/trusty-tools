Changed
- Tests: nine previously `#[ignore]`d tests (filesystem, redb, fail-open, `cargo` probe) now run by default; the two recall-ranking tests use the real ONNX embedder in the pre-publish gate instead of silently seeding the mock; every remaining ignored test states its reason (refs #8787 audit).
