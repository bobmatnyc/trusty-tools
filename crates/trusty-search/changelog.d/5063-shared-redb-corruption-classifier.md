Changed

- `core::corpus_recovery::is_incompatible_corpus_format` and
  `backup_incompatible_corpus` now delegate to
  `trusty_common::redb_open::is_incompatible_format` and
  `backup_incompatible_file`, and `INCOMPATIBLE_CORPUS_SUFFIX` aliases
  trusty-common's constant (#5063). Same verdict and same quarantine path —
  suffix plus numbered anti-clobber fallback — for every input. The corpus
  recovery policy is unchanged and stays here: it fails CLOSED on genuine
  corruption (#4227) via `is_genuine_corpus_corruption`, which remains
  trusty-search's own predicate, and it opens with a tuned page-cache size.
