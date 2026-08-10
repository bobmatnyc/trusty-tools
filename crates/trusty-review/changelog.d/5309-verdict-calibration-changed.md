Changed

- `pipeline::finding_hygiene::HygieneCounts` gained a fourth per-pass counter and is now
  `#[non_exhaustive]`. Construct it with `..Default::default()` from outside the crate.
  Both land in this release together so adding a fifth hygiene pass is not a further
  break (#4088).
