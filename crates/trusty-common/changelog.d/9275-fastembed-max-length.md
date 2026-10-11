Changed
- `FastEmbedder` passes `max_length` 256 to fastembed, so every all-MiniLM-L6-v2 input truncates at 256 tokens, as in sentence-transformers. Before, fastembed truncated at the cached `tokenizer_config.json` `model_max_length`: 512 on hosts holding `Qdrant/all-MiniLM-L6-v2-onnx` revision `5f1b8cd78bc4`, 256 on hosts that downloaded it after 2026-09-27 (#9275).
