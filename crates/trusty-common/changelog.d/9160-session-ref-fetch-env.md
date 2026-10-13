Added
- `catchup::session_refs::hydrate_session_cache_with_env` sets a caller-supplied env (for example a pinned gh token and a `GIT_CONFIG_*` credential-helper pin) on the hydration's one network call, the `git fetch`. `hydrate_session_cache` is unchanged and delegates with no env (#9160).
