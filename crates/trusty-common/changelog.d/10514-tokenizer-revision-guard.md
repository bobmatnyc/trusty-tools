Security

- `ChunkTokenizer::for_fastembed_model` and `for_fastembed_model_in` now reject a model-cache `refs/main` revision that is empty, contains `/`, `\` or `..`, or uses a character outside `[A-Za-z0-9._-]`, returning the new `ChunkError::InvalidRevision` before any tokenizer file is read (#10514)
