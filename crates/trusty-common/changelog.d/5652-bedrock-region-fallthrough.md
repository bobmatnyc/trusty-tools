Fixed

- `bedrock_region_resolution` no longer fails when `AWS_REGION` is set in the
  ambient environment
  ([#5652](https://github.com/bobmatnyc/trusty-tools/issues/5652)). The test
  asserted that an empty explicit region reaches `us-east-1`, which skips the
  `TRUSTY_AWS_REGION` and `AWS_REGION` tiers that `resolve_bedrock_region` is
  documented to consult first. The precedence walk moved into a pure
  `resolve_region_from(explicit, trusty_env, aws_env)` helper that the test
  drives directly, so every tier is covered without reading or mutating
  process-wide env vars. `resolve_bedrock_region`'s behaviour is unchanged.
