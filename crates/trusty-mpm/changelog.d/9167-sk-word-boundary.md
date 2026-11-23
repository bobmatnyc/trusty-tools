Fixed
- The credential scrubber no longer redacts a path segment containing `sk-` inside a word (such as `task-roundtrip-…`) as an API key; `sk-` keys are still redacted.
