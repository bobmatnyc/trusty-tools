Documentation
- Fixed the stale doc comment on `SHRINK_GUARD_RATIO_DIVISOR`
  (`core::store::usearch_store`), which still described the periodic HNSW
  persister's checkpoint race as open work tracked by #3970. #3970 is closed;
  #3975 shipped the staged-write-then-swap the comment cited as a
  recommendation, and the comment now describes that fix (#6202).
