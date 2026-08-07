Breaking

- `DedupStore`'s `claim`, `complete`, and `release` are now `async` and take
  `&Arc<Self>`; the synchronous forms are renamed `claim_blocking`,
  `complete_blocking`, and `release_blocking`. The blocking forms wait on redb's
  file lock, so calling one from an async task stalls a runtime worker — the
  async forms move that wait to a blocking thread. `DedupError` gains a
  `Contended` variant and is now `#[non_exhaustive]`: an exhaustive `match` on
  it needs a wildcard arm, and in exchange future variants stop being breaking
  changes. Construction of existing variants is unaffected (#5064).
