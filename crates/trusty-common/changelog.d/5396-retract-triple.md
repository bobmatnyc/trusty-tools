Added

- `KgStoreRedb::retract_triple(subject, predicate, object)` and its async counterpart `KnowledgeGraph::retract_triple` close ONE object at a `(subject, predicate)` pair and leave the other objects live ([#5396](https://github.com/bobmatnyc/trusty-tools/issues/5396))
  - `retract(subject, predicate)` closes every object at the pair, so removing one wrong object took the correct siblings with it. Re-asserting was no escape either: `is-a`, `works-at`, `uses` and `depends-on` are absent from `kg_store::FUNCTIONAL_PREDICATES`, so a new object joins the wrong one instead of superseding it.
  - Naming an object that is not active at the pair returns 0 rather than an error, so a cleanup pass can be re-run over the same candidate list.
  - A functional predicate gets no special treatment — the row is addressed by its full `(subject, predicate, object)` key, so an object the caller did not name is never closed.
  - `#4810` had recorded the three-argument shape as a deliberate non-goal; that doc comment on `retract` now points at the new method.
