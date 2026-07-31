Changed

- the activity classifier now issues its LLM call through the unified `trusty_common::inference::InferenceAdapter` instead of the legacy `chat::ChatProvider` SSE pump ([#4427](https://github.com/bobmatnyc/trusty-tools/issues/4427))
  - One blocking `InferenceAdapter::chat` call replaces the `chat_stream` +
    mpsc pump. The classifier only ever consumed the finished string, so
    streaming bought nothing and cost a truncated-mid-stream failure mode
    (#3757) that had to be detected and re-reported by hand.
  - Credentials now resolve through the shared ladder (env > `.env.local` >
    secure store) rather than a direct `OPENROUTER_API_KEY` env read. A daemon
    whose key lives only in `.env.local` or the secure store — and which
    previously reported "OPENROUTER_API_KEY is not configured" — now classifies
    normally. The `MissingApiKey` error variant and its message are unchanged,
    so every caller's degrade branch behaves as before.
  - Per-check cost metrics carry real token counts. The SSE path could not see
    usage and hard-coded `(0, 0)`, which made every activity cost tally read
    zero.
  - `OpenRouterClassifier` moved from `activity::monitor` to a new
    `activity::classifier` module (both files were headed past the 500-SLOC
    cap); it is re-exported from `crate::activity`, which is now the stable
    import path.
