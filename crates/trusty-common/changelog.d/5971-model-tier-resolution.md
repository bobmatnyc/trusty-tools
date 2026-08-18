Added

- `inference::ModelTier` maps a capability tier plus a `ProviderId` to a
  concrete model id, so a consumer asks for the class of model its work needs
  instead of pinning a version-stamped string that never moves when the model
  behind a role moves. Three tiers: `Analysis`, `Interaction`, `Haiku`. Analysis
  and Interaction both resolve to Opus 4.8 today and stay separate variants so
  they can diverge later.
- `ModelTier::resolve` returns `Option`, not `Result`. A provider with no
  verified id for a tier returns `None`, which means "no tier default here" and
  leaves the caller on whatever default it already had. AWS Bedrock's opus tiers
  are deliberately unmapped: the inference-profile id could not be verified, and
  its shape is not derivable from the family name — this table shows both
  directions, with Sonnet 4.6 working bare while Haiku 4.5 needs a date stamp
  and a `-v1:0` suffix.
