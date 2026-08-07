Removed

- **BREAKING — the next release of this crate must be `0.9.0`, not `0.8.x`.**
  Removed the fastembed/ONNX neural clustering embedder and, with it, public
  API: `EmbedderKind::Neural`, `embedder::NeuralEmbedder`, the
  `bundled-ort` / `load-dynamic` / `cuda` Cargo features (`default` is now
  `["http-server"]`), and `ClusterQueryParams::method`'s type (now
  `Option<String>`, validated in the handler). CI cannot detect a SemVer break
  (#4088 — the gap that got 0.7.3 yanked), so this line is the record a
  releaser has to act on. Nothing selected `method=neural` —
  `trusty-console`, the `cluster_concepts` MCP tool and the embedded UI all
  used the `bow` default — yet the daemon constructed the model at every boot,
  and the untimed Hugging Face request that construction made blocked the
  listener for as long as the request took (31m46s in one production boot;
  reproduced at 60.17s and 120.13s against a stub HF endpoint with matching
  injected delays, versus 0.20s after the fix). `bow` is now the sole
  embedder, `--fastembed-cache` is an accepted no-op so existing launchd
  plists keep starting, and `?method=neural` returns 400 instead of BOW
  vectors labelled `neural` (#5067)
