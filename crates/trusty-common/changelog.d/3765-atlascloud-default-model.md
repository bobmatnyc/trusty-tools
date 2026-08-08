Changed

- AtlasCloud's seeded `default_model` is now `deepseek-ai/deepseek-v4-flash`,
  replacing `openai/gpt-5.6-sol`
  ([#3765](https://github.com/bobmatnyc/trusty-tools/issues/3765)).
  A live probe found AtlasCloud gates its catalog by account PLAN, not only by
  key validity: a Coding-Plan key answers `403 invalid token for coding plan`
  for `openai/gpt-5.6-sol` and most of the catalog even though `GET /v1/models`
  lists them, so "just pick AtlasCloud" failed on a valid key. The replacement
  was verified live on such a key — a real completion and a real OpenAI-style
  `tool_calls` response — and has a 1,048,576-token context with the cheapest
  rates of the callable set. It is Coding-Plan-informed, which the seed comment
  records; `max_context_window` is unchanged at 1,050,000 because it is the
  provider-level fallback, not this model's own window.
