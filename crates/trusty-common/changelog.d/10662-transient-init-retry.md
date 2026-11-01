Fixed
- `FastEmbedder` no longer loads the other model when the primary model's init fails on a transient error such as a held hf-hub blob lock. It retries up to 5 times, and then fails with a named `embedder init failed` error. Non-transient errors still fall back (#10662).
