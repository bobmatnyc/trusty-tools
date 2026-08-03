Added

- `fuzzy_identity_fallback` config key (#4251). Unset (default) disables the
  Tier-3/4 fuzzy fallback only when a declared `aliases_file` successfully
  loaded a non-empty alias table; `true` forces it on, `false` forces it off.
  A declared `aliases_file` that fails to load, or resolves empty, leaves the
  fallback ON and logs at `error!` rather than silently fragmenting every
  identity. `IdentityResolver` gained the matching `with_fuzzy_fallback()`
  builder and `fuzzy_fallback()` accessor for callers that construct a
  resolver outside `from_config`.
- The behaviour flip is announced once at `info!` when it takes effect.
