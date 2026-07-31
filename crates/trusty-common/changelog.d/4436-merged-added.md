Added

- **`inference::streaming::StreamAssembly` — rebuild a `ChatResponse` from
  streamed events (issue #4425).** The inverse of the existing
  `buffered_stream`: push `ChatStreamEvent`s in arrival order, then
  `into_response()` yields the same response the buffered `chat()` path would
  have returned for that turn — text concatenated, tool-call fragments merged
  by `index`, finish reason and usage carried from the terminal `Done`. It is a
  pure synchronous accumulator taking no callback, so the caller keeps its poll
  loop and may `await` arbitrary work (rendering a delta to a UI) between
  pushes. Without it, every consumer adopting `chat_stream` had to hand-roll
  the same three-part bookkeeping — the reason trusty-code's streaming
  migration looked like an agent-loop rewrite rather than one swapped call.
- **`InferenceError::MissingConfig(String)` (issue #4425).**
  `MissingCredential` can only name a `ProviderId` — it carries no
  operator-actionable message and cannot describe a missing NON-credential
  setting (an unset region, an unconfigured model slug). trusty-code's
  migration onto this enum would otherwise have had to flatten those messages
  into `Unsupported`, whose display text ("unsupported inference capability:
  OPENROUTER_API_KEY not set") actively misleads. Classifies as an alarm and
  never as retryable, matching `MissingCredential`. **Breaking for exhaustive
  matches** over `InferenceError` (the enum is not `#[non_exhaustive]`);
  in-tree consumers were verified to build.
- **`PromptTokensDetails` and `UsageBlock` are re-exported at the `inference`
  root.** A consumer that owns a `ChatResponse` owns its wire usage block;
  reading it previously meant reaching into `inference::types::usage`.
- **`InferenceAdapter::capabilities_for(model)` — the model-aware capability
  accessor (issue #4425).** `capabilities()` assumes one adapter serves one
  provider, but a ROUTING adapter picks its backend per request from the model
  slug (trusty-code's `OpenAiCompatClient` spans OpenRouter / Fireworks /
  Together / AtlasCloud; its `DispatchingLlmClient` adds Bedrock). Such an
  adapter could only answer with one hard-wired provider's profile, which is
  silently wrong for every other backend it serves — the OpenRouter-only usage
  directive, `cache_control` support, the tool dialect, and the context-window
  fallback tier all differ. `capabilities_for` defaults to `capabilities()`, so
  every single-provider adapter is unaffected; a routing adapter overrides it to
  resolve through the same gate its `chat`/`chat_stream` use.
  **`context_window`'s default now derives its provider tier from
  `capabilities_for(model)`** rather than `capabilities()` — a behaviour change
  only for adapters that override `capabilities_for` (none did before this
  release). Not a breaking change: both are defaulted trait methods.
