Documentation

- **Module docs render once instead of twice.** Four modules carried both an outer `///` on their `mod x;` declaration and their own inner `//!`; rustdoc concatenates the two, so each module page showed two summary lines and two Why/What/Test triples. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
  - two were merged rather than deleted: `audit::real_binary_tests` (the outer alone recorded that the module is Unix-only because it drives the binary through a `#!/bin/sh` wrapper) and `collect::bitbucket::tests` (the `#[path]` resolution mechanism)
