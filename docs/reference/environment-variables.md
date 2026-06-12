# Environment Variables Reference

| Variable | Required by | Purpose |
|---|---|---|
| `OPENROUTER_API_KEY` | `trusty-search` `/chat`, `trusty-common` chat helpers, `trusty-analyze` deep pass (OpenRouter path) | LLM chat via OpenRouter. Pass as argument to library helpers; never read from env inside library crates. Required for `POST /analyze/deep` unless a `bedrock/<model-id>` model is selected. |
| `TRUSTY_LLM_MODEL` | `trusty-analyze` deep pass | LLM model id for the deep-analysis narrative pass. Default: `openai/gpt-4o-mini` (OpenRouter). Set to `bedrock/<bedrock-model-id>` (e.g. `bedrock/us.anthropic.claude-sonnet-4-6`) to route through AWS Bedrock instead of OpenRouter. The `bedrock/` prefix selects the Bedrock provider; anything else routes to OpenRouter. Claude Sonnet 4.6 uses the short form without date stamp or `-v1:0` suffix. |
| `TRUSTY_AWS_REGION` | `trusty-analyze` (Bedrock deep pass) | AWS region for Bedrock `Converse` calls. Takes priority over `AWS_REGION`. Default: `us-east-1`. |
| `AWS_REGION` | `trusty-analyze` (Bedrock deep pass) | Fallback AWS region for Bedrock calls. Overridden by `TRUSTY_AWS_REGION`. |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` | `trusty-analyze` (Bedrock deep pass) | Standard AWS credentials for Bedrock access. The full AWS credential chain (env vars, `~/.aws/credentials` profiles, IAM roles, SSO) is supported. No API key is needed when using a `bedrock/` model. |
| `RUST_LOG` | all daemons | Tracing filter, e.g. `RUST_LOG=debug` or `RUST_LOG=trusty_search=debug,warn`. |
| `TRUSTY_MEMORY_LIMIT_MB` | `trusty-search` | Soft RSS ceiling for indexing pipeline. Auto-tuned from system RAM; override only when needed. |
| `TRUSTY_MAX_CHUNKS` | `trusty-search` | Hard cap on chunks per index. Auto-tuned; rarely set manually. |
| `TRUSTY_MAX_BATCH_SIZE` | `trusty-search` | ONNX embedding batch size. Auto-tuned; set if OOM during reindex. |
| `TRUSTY_EMBEDDING_CACHE` | `trusty-search` | LRU embedding cache capacity (entries). |
| `TRUSTY_COREML_TRIPWIRE_MB` | `trusty-search` (Apple Silicon) | RSS-delta ceiling per CoreML batch (default 4 GB). If exceeded, batch size is halved automatically. Override for hosts with different memory pressure characteristics. |
| `TRUSTY_GPU_MEM_LIMIT_BYTES` | `trusty-search` / `trusty-embedderd` (CUDA EP, issue #600) | Exact CUDA `gpu_mem_limit` in bytes, applied alongside `arena_extend_strategy=kSameAsRequested` to stop ORT's BFCArena over-reserving VRAM and OOMing a 16 GB Tesla T4. Default 12 GiB (`12884901888`). Takes precedence over `TRUSTY_GPU_MEM_LIMIT_MB`; a malformed or `0` value is ignored. Removes the need for the old `TRUSTY_MAX_BATCH_SIZE=32` workaround. |
| `TRUSTY_GPU_MEM_LIMIT_MB` | `trusty-search` / `trusty-embedderd` (CUDA EP, issue #600) | CUDA `gpu_mem_limit` in megabytes (scaled by 1024²). Used only when `TRUSTY_GPU_MEM_LIMIT_BYTES` is unset/invalid. E.g. `6144` for an 8 GB card. |
| `ORT_DYLIB_PATH` | `trusty-search` (CUDA, glibc < 2.38); `trusty-analyze` (`load-dynamic`, `cuda` features) | Path to `libonnxruntime.so` on hosts with glibc < 2.38 and CUDA builds. For trusty-analyze, install with `--no-default-features --features http-server,load-dynamic` and set this var to the system libonnxruntime path. |
| `SKIP_UI_BUILD` | `trusty-search` `build.rs` | Set to `1` to skip the Svelte UI build step (CI publish flows). |
| `TRUSTY_NO_KG` | `trusty-search` daemon | Machine-wide default for `skip_kg`. When set to `1`, `true`, or `yes`, every new index created via `POST /indexes` (or `trusty-search index`) has `skip_kg=true` applied automatically unless the caller explicitly sets `skip_kg: false`. Useful for CI machines or resource-constrained hosts where KG is never needed. |
