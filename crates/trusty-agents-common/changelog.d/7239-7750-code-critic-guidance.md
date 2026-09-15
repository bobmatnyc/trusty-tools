Changed

- `code-critic` agent body now emits the enclosing function or method name
  beside every finding's file + line citation
  (refs [#7239](https://github.com/bobmatnyc/trusty-tools/issues/7239))
- `code-critic` now greps for a changed fixture-of-record file's consumers
  (seed fixtures, Terraform demo data, content YAML) and runs their suites
  before verdict, or states in Notes that none exist, instead of gating on
  the changed file's own language tooling alone
  (refs [#7750](https://github.com/bobmatnyc/trusty-tools/issues/7750))
- `nextjs-engineer` body points to the `test-driven-development` skill's new
  Prop-Removal Checklist for a `...rest`-forwarding component
  (refs [#7371](https://github.com/bobmatnyc/trusty-tools/issues/7371))
