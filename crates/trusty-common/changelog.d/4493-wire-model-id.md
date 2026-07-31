Fixed

- A model slug's provider prefix no longer reaches a direct provider on the wire
  (closes [#4493](https://github.com/bobmatnyc/trusty-tools/issues/4493)).
  `provider_for` CONSUMES the `<prefix>/` marker to route — so with an
  `OPENAI_API_KEY` present, `openai/gpt-4o-mini` routed to OpenAI-direct — but the
  slug then travelled into the request body unchanged, and `api.openai.com` 400s
  on a model id it does not publish. The new `ProviderId::wire_model_id` removes
  exactly one leading marker, and only when it names that provider, so a slash
  inside a real model id survives (`accounts/fireworks/models/…`,
  `meta-llama/…`) and a nested vendor segment does too
  (`atlascloud/openai/gpt-5.6-sol` → `openai/gpt-5.6-sol`). OpenRouter is exempt
  and still transmits the full `vendor/model` slug verbatim — it routes by that
  slug and serves first-party models under a genuine `openrouter/` vendor. The
  two adapters that had each hand-rolled this strip for one provider
  (Bedrock, Anthropic-direct) now delegate to the shared rule.
