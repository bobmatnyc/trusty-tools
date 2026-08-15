Documentation

- **Module docs render once instead of twice.** `intent::route` carried both an outer `///` on its `mod` declaration and its own inner `//!`; rustdoc concatenates the two, so the module page showed two summary lines and two Why/What/Test triples. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
  - merged rather than deleted: only the outer explained why routing is a separate module from `intent::classify_intent` — the classifier decides how much machinery an input needs, while `route` decides which backend receives it once the answer is already "hand this off"
