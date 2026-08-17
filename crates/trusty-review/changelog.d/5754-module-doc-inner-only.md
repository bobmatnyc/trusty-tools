Documentation

- **Module docs render once instead of twice.** `config::resolve_index_tests` carried both an outer `///` on its `mod` declaration and its own inner `//!`; rustdoc concatenates the two, so the module page showed the split rationale twice. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
