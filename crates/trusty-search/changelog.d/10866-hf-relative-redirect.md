Fixed
- The `candle` embedder's first-use model download no longer fails with `RelativeUrlWithoutBase`: hf-hub moves to the workspace 0.5 pin, which follows HuggingFace's relative 307 redirect. `HF_HOME` is still honoured (#10866).
