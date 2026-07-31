Added

- **`inference::bedrock` streams natively via `ConverseStream` (issue #4426).**
  `BedrockAdapter` now overrides `InferenceAdapter::chat_stream` with AWS's
  real streaming operation instead of inheriting the trait's buffered fallback,
  so a `bedrock/*` turn arrives token-by-token rather than as one delta emitted
  after the model already finished. The event handling is ported from the
  implementation proven in production on `chat::bedrock_impl` (#3767) and
  extended: `ContentBlockDelta::Text` → `ChatStreamEvent::Delta`, tool-use
  block starts and partial-JSON argument fragments → `ChatStreamEvent::ToolCall`
  (which the `chat::ChatProvider` path never supported), `MessageStop` +
  `Metadata` folded into the single terminal `Done` carrying the finish reason
  and token tally, and a mid-stream failure surfacing as a terminal `Err` and
  never as a `Done`. Both transports build their request from ONE shared
  conversion (`build_converse_parts`), so streamed and buffered turns cannot
  disagree about messages, sampling, tool config, or the reported
  `finish_reason`. Consumers that delegate `chat_stream` to the shared adapter —
  trusty-code's `BedrockChatClient` — get this with no change of their own.
