Changed

- The run index's investigation-coverage section now says when its figure is a
  sample rather than the whole estate: a partial pass renders a
  `**Sampled, not exhaustive.**` line naming the sample size, the population and
  the unread remainder. A run that read every tracked file renders exactly what
  it did before. New public items `grounding::coverage_rollup::Sampling`,
  `RepoCoverage::sampling` and `Rollup::sampling` carry the fact; no existing
  signature or struct shape changed (issue #6138).
