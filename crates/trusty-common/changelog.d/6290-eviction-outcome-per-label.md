Added

- `launchd_labels::EvictionOutcome` and `LabelEviction` report per label whether
  a launchd unit was evicted, was already absent, or FAILED to go down.
- `LaunchdConfig::evict_legacy_detailed` returns those outcomes. `evict_legacy`
  keeps its signature and now reports only labels that were genuinely evicted.
