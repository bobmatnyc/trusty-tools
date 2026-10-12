Fixed
- `memory_recall` and `memory_recall_deep` with a `wing` no longer return another wing's drawer from the essential layer (L1), on the ready path and while the embedder warms (#9719). The `wing` argument's schema description now says L1 is scoped.
