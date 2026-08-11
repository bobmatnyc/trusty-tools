Added
- **`kg-rebuild --merge-punctuated-twins`** folds a pre-#4678 punctuated entity
  node onto its cleaned twin, so `` `redb` `` and `redb` stop being two nodes for
  one thing. #4678's edge trim fixed extraction going forward and split every
  entity already in the graph; nothing removed the old node, because the four
  pattern predicates are absent from `FUNCTIONAL_PREDICATES` (an `assert` adds
  the cleaned spelling beside the punctuated one) and `--purge-stale-subjects`
  only selects subjects `is_stop_token` rejects, which a real entity never trips.
  Each rebuild over the same drawer content widened the split.

  This is a merge, not a delete: every auto-extracted triple is re-asserted under
  the cleaned identity and only then retracted at the punctuated one, in BOTH the
  subject and the object position, so the merged node keeps its own pre-existing
  triples and gains the re-pointed ones. Object-position re-pointing is what
  needed #5396's `retract_triple` — closing the whole `(subject, predicate)` pair
  would have taken the punctuated object's correct siblings with it.

  Off by default, prints every move, and takes `--dry-run` (which now gates on
  either maintenance flag rather than on `--purge-stale-subjects` alone) to
  report the list while writing nothing. Selection is as narrow as the purge's: a
  term under the `drawer:`/`tag:`/`topic:`/`room:` namespaces never moves, only
  triples stamped `auto:remember` are rewritten, and a punctuated stopword is
  left to the purge — `is_stop_token` partitions the two passes so no term is
  both deleted and re-pointed. Re-running is a no-op.
