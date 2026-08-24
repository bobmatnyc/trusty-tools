Fixed

- A per-request model override now reaches the provider. `OpenRouterProvider`
  and `FireworksProvider` serialised the id they were constructed with and
  dropped `LlmRequest.model`, so a request naming a different model ran on the
  constructor's model and the response echoed the wrong id with nothing
  reporting the substitution; `BedrockProvider` already read `req.model`. All
  three now resolve the same way through `LlmRequest::effective_model`, which
  returns the request's id and falls back to the constructor's only when the
  request names none. `OpenRouterProvider::with_base_url` is new — the
  endpoint was a hardcoded constant, so nothing could assert on the bytes the
  provider actually sends; the regression tests now read the model out of a
  mock server's received body.
