Fixed

- `KgStoreRedb::assert` and `apply_batch`'s `Assert` op already shared one
  close-and-replace implementation (`batch_assert`) as of the #4810 triple-key
  rework — this closes the remaining gap #4922 found: `kg_writer.rs`'s module
  and `commit_and_reply` doc comments claimed a single queued op falls back to
  "the matching single-op method," but `commit_and_reply` calls
  `KgStoreRedb::apply_batch` unconditionally, for every batch size from 1
  upward. The comments now describe that unconditional path; the single-op
  methods on `KgStoreRedb` are reachable only via `KgWriter::bypass` (no
  running actor) and the actor's own closed-channel fallback, never from the
  coalescing loop. Added
  `single_assert_and_apply_batch_one_op_agree_on_valid_from`, which pins that
  the sync single-op path and a one-element `apply_batch` produce
  byte-identical stored triples (`valid_from`/`valid_to` included) — the
  agreement PR #4920's Tier S `affirmed_at` derivation depends on.
