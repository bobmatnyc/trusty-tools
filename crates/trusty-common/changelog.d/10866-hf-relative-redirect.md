Fixed
- The candle embedder's first-use model download no longer fails with `RelativeUrlWithoutBase`: hf-hub moves to 0.5, which follows HuggingFace's relative 307 redirect. `HF_HOME` is still honoured, and concurrent first-use downloads in one process are serialized (#10866).
