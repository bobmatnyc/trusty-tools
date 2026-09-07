Changed

- `agent_loop`'s inference-error log label handles `InferenceError::UnsupportedCapability` as its own class, `unsupported_capability`, rather than folding it into `unsupported` — the two describe different problems, and an operator grouping by this label is looking at a request routed to a provider whose capabilities cannot serve it. Every `ChatRequest` literal in the crate names the new `response_schema` field explicitly as `None`; behavior is unchanged. Refs #5588
