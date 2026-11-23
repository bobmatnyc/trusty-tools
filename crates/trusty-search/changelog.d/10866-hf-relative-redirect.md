Fixed
- The `candle` embedder's first-use model download no longer fails with `RelativeUrlWithoutBase`: hf-hub moves to the workspace 0.5 pin, which follows HuggingFace's relative 307 redirect. `HF_HOME` is still honoured (#10866). Candle-only builds now verify TLS against bundled Mozilla roots instead of the system trust store, so custom or corporate CAs are not trusted on that path.
