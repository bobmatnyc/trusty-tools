Changed
- The shipped review voice is now the neutral `house` voice at `voices/house/voice.toml`, replacing the previous org-derived voice; select it with `[voice] package = "house"`. A config that still names `duetto` loads `house` with a deprecation warning.
- Release order: publish this trusty-review release before content bundle 2.7.0. Older trusty-review binaries require `voices/duetto/voice.toml` in `content check`, and the 2.7.0 bundle no longer ships it.
