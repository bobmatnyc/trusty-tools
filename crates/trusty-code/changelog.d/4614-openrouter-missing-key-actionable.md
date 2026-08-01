Fixed

- **A missing `OPENROUTER_API_KEY` again reports an actionable error (issue
  #4614).** OpenRouter is tcode's default route, so it is the first credential
  a new operator has to set — yet since #4436 deleted the `convert::map_error`
  bridge, an absent key surfaced as the shared resolver's bare
  `MissingCredential { provider: OpenRouter }` ("no credential resolved for
  provider openrouter"), which names nothing the operator can act on.
  `build_adapter` now guards the OpenRouter route explicitly, exactly as it
  already did for Fireworks, Together, and AtlasCloud, and returns
  `MissingConfig` naming the env var and the three ways to set it. This is the
  contract both `llm::client::build_adapter` and `llm::dispatch::chat` have
  documented throughout; only the code had drifted.
- **The missing-credential tests no longer pass without asserting anything
  (issue #4614).** All four `missing_*_key_errors_*` tests returned early
  whenever the corresponding API key was present in the ambient environment, so
  on any developer machine holding a real key they reported `ok` having
  executed zero assertions — which is why the regression above went unseen
  locally and was visible only in CI. Each test now clears its own key for the
  duration of the test body (`#[serial]`, restored on drop) so the assertions
  run unconditionally, and no live API call is reachable.
