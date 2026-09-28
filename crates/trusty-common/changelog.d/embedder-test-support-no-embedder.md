Changed
- The `embedder-test-support` feature no longer implies `embedder`. On its own it compiles nothing and pulls no ONNX; it exposes `MockEmbedder` and `seed_shared_embedder_with_mock` only alongside `embedder` or `memory-core`. A crate that enabled `embedder-test-support` alone to get the embedder must now name `embedder` as well — every in-workspace consumer already does.
