Added

- `inference::shape` (feature `inference-client`): `classify_model_shape`
  answers which provider a model id belongs to from its spelling, and how
  strongly. `accounts/fireworks/models/…` is Fireworks, any other
  `vendor/model` slug is OpenRouter, and a region-scoped inference profile is
  Bedrock — all `ShapeEvidence::Conclusive`, because each belongs to exactly one
  catalogue. A dotted `vendor.model` id whose first segment is a known Bedrock
  vendor is `Probable`: a vendor-name guess, not a catalogue fact.
  `infer_provider_from_model_shape` is the same answer with the evidence
  dropped, and returns `None` for an id that could be several providers'.
  Two reconciliation checks read that: `shape_mismatch` reports any disagreement
  with the provider about to execute the call, and `conclusive_shape_mismatch`
  reports only the catalogue-fact kind. Which to call depends on where the
  provider came from — a standing config default loses to any shape, while an
  explicit per-call routing prefix is a human's statement about that call and
  loses only to a catalogue fact. Call either only after stripping your own
  routing prefixes; the same `anthropic/…` string is an OpenRouter slug to this
  module and the Anthropic first-party family to `ProviderId::from_slug_prefix`.
  `BEDROCK_INFERENCE_PROFILE_PREFIXES` moves here so trusty-review's Bedrock
  model-id validator and this inference cannot disagree about the same string.
