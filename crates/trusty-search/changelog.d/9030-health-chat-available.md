Added
- `search.health` and `GET /health` now carry `chat_available: bool`, true only when a chat provider (local Ollama / LM Studio, or an OpenRouter key) exists, so the console can hide a chat that would answer 503; it is false when no provider exists or resolution does not finish within 2 s ([#9030](https://github.com/bobmatnyc/trusty-tools/issues/9030))
