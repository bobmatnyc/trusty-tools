Breaking

- `DedupStore`'s `claim`, `complete`, and `release` are now `async` and take
  `&Arc<Self>`; the synchronous forms are renamed `claim_blocking`,
  `complete_blocking`, and `release_blocking`. The blocking forms wait on redb's
  file lock, so calling one from an async task stalls a runtime worker — the
  async forms move that wait to a blocking thread. `DedupError` gains a
  `Contended` variant, which breaks an exhaustive `match` on it (#5064).
