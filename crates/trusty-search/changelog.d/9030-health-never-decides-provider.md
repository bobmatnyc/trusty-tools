Fixed
- `GET /health` and `search.health` no longer decide the chat provider: a poll made at boot, before a local model server is up, used to lock `search.chat` into a 503 (or into OpenRouter over local) until restart; health now reads the provider cell when filled and otherwise probes without writing it ([#9030](https://github.com/bobmatnyc/trusty-tools/issues/9030))
