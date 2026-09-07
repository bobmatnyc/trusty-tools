Fixed

- A `local/*` or `ollama/*` model slug reached OpenRouter. `provider_for_slug` had no Local branch and `build_adapter` matched only Fireworks, Together and AtlasCloud, so every other provider — Local included — fell through to the OpenRouter builder; a local turn was billed to the cloud, or failed asking for `OPENROUTER_API_KEY`. Both slugs now select `ProviderId::Local`, the routing marker is stripped before the id reaches the server, and the adapter is `trusty_common::inference::providers::local`'s, which probes the server inside a one-second budget before each request
