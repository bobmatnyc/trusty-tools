Fixed

- `FastEmbedder::new` now resolves its ONNX model cache through
  `trusty_common::embedder::resolve_fastembed_cache_dir` instead of a hard-coded
  `~/.cache/trusty-agents/models`, so `FASTEMBED_CACHE_DIR` /
  `FASTEMBED_CACHE_PATH` reach it and the model is shared with every other
  trusty-* embedder consumer rather than downloaded a second time. The old path
  was unreachable by any env var, which is why the pre-publish gate re-fetched
  the model from HuggingFace on every run despite pre-seeding the cache
  ([#812](https://github.com/bobmatnyc/trusty-tools/issues/812)). Existing
  installs re-download the ~23MB model once, into the shared location.
- Model init retries a transient fetch failure — HTTP 429 from the hub's rate
  limiter, or hf-hub's `Lock acquisition failed` when another process holds the
  blob lock — with exponential backoff and per-process jitter, up to five
  attempts. A terminal error (missing file, 404, corrupt model) still fails on
  the first attempt rather than costing four pointless retries. This covers
  first start on a user's machine as well as CI.
