Fixed

- **A failed `chat_stream` handshake no longer punches a hole in the
  debug-capture turn sequence (issue #4425, review finding 1).**
  `DebugCaptureLlmClient::chat_stream` reserves its `turn_index` before opening
  the stream, and an open failure used to propagate with `?` — so the index was
  consumed but never written, leaving a permanent GAP in
  `TCODE_DEBUG_TRANSCRIPT`'s sequence and losing the only record of the request
  that failed. The blocking `chat` path records in all cases; the streaming path
  now matches it exactly. Pinned by
  `stream_open_failure_records_its_reserved_turn_index`, which asserts the
  recorded indices are contiguous from zero across a failed stream-open and a
  following successful turn.
- **A streaming failure is captured with its ORIGINAL error variant, not a
  re-wrapped `Transport` (issue #4425, review finding 2).** The stream's error
  branch synthesised `InferenceError::Transport(e.to_string())` for the record —
  the one variant that is always retryable and never an alarm — so a
  missing-config or auth failure appeared in the transcript as a transient
  network blip and `is_retryable`/`is_alarm` became underivable from the
  capture. The record now carries the real error, and each failure record gains
  explicit `error_retryable` / `error_alarm` booleans so classification is
  readable without re-parsing Display text (the record cannot hold an
  `InferenceError` directly — the shared enum is not `Serialize`).
- **A multi-provider adapter answers capability questions for the provider the
  request actually routes to (issue #4425, review finding 3).**
  `OpenAiCompatClient` and `DispatchingLlmClient` both hard-wired
  `capabilities()` to OpenRouter's profile even though they route per request
  across Fireworks, Together, AtlasCloud, and Bedrock — reporting
  `detailed_usage_accounting: true` and OpenAI-dialect tooling for backends that
  honour neither, and handing compaction OpenRouter's 200K context tier for a
  128K backend. Both now override the new model-aware
  `InferenceAdapter::capabilities_for(model)`, resolving through the SAME gate
  their `chat`/`chat_stream` route on, so capabilities can never disagree with
  where the request is sent. `BedrockChatClient` and the
  `delegating_adapter_identity!` decorators forward it, so a recorder or
  debug-capture wrapper cannot collapse the routing back to one provider. The
  model-free `capabilities()` still answers for the routing default — the
  backend an unprefixed slug genuinely reaches — and is documented as such.
  #4426 builds Bedrock streaming on this surface.
