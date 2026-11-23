Fixed
- The credential scrubber no longer redacts a path segment containing `sk-` inside a word (such as `task-roundtrip-…`) as an API key; `sk-` keys are still redacted, including after `_` (`OPENAI_KEY_sk-…`) or a JSON-escaped `\n`, and the character before the key is kept.
