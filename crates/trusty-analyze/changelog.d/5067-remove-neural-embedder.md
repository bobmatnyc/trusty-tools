Removed

- The fastembed/ONNX neural clustering embedder, its `bundled-ort` /
  `load-dynamic` / `cuda` features, and the `fastembed` dependency. Nothing
  selected `method=neural` — `trusty-console`, the `cluster_concepts` MCP tool
  and the embedded UI all used the `bow` default — yet the daemon constructed
  the model at every boot, and the untimed Hugging Face request that
  construction made blocked the listener for as long as the request took
  (31m46s in one production boot; reproduced at 60.17s and 120.13s against a
  stub HF endpoint with matching injected delays, versus 0.20s after the fix).
  `bow` is now the sole embedder, `--fastembed-cache` is an accepted no-op, and
  `?method=neural` returns 400 instead of BOW vectors labelled `neural` (#5067)
