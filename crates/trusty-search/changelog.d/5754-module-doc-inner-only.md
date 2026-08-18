Documentation

- **Module docs render once instead of twice.** 11 modules carried both an outer `///` on their `mod x;` declaration and their own inner `//!`; rustdoc concatenates the two, so each module page showed two summary lines and two Why/What/Test triples. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
  - no prose was lost: each pair was read on both sides and the outer removed only where every fact already appeared in the inner
  - `service_unit::launchd_unit_tests` was the one place the two sides were byte-identical — its macOS-gating paragraph appeared verbatim twice
  - links in the merged doc used to resolve against the parent module's scope, which is what broke 452 of the 852 intra-doc links repaired in #5744; inner-only removes that trap rather than working around it
