Changed

- The REPL's `/provider local` and `/local`, the ctrl-turn fast-path availability check, and the `/api/models` catalog all route through `trusty_common::local_probe` instead of three private Ollama probes that disagreed on the budget (2s, 500ms) and on the endpoint (`/api/tags`). One request to `/v1/models` now answers both liveness and the model list, which also covers LM Studio and vLLM; the four `OLLAMA_HOST` readers collapse to one, so the probe and the request that follows it cannot dial different machines
