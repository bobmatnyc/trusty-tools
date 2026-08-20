Added

- `inference::shape` (feature `inference-client`): `infer_provider_from_model_shape`
  answers which provider a model id belongs to from its spelling —
  `accounts/fireworks/models/…` is Fireworks, any other `vendor/model` slug is
  OpenRouter, and a region-profiled or dotted `vendor.model` id is Bedrock —
  returning `None` for an id that could be several providers'.
  `shape_mismatch` turns that into the check a resolver needs before it runs a
  call: `Some(inferred)` when the id names a provider other than the one about
  to execute it. Call it only after stripping your own routing prefixes; the
  same `anthropic/…` string is an OpenRouter slug to this module and the
  Anthropic first-party family to `ProviderId::from_slug_prefix`.
  `BEDROCK_INFERENCE_PROFILE_PREFIXES` moves here so trusty-review's Bedrock
  model-id validator and this inference cannot disagree about the same string.
