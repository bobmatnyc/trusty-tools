Changed

- `tm-workflow` skill's merging-stage rule now covers a fixture-of-record
  file (seed fixture, Terraform demo data, content YAML) the same way it
  covers a changed public interface: grep for consumers and run their suites
  before the critic verdict, or state that none exist
  (refs [#7750](https://github.com/bobmatnyc/trusty-tools/issues/7750))
- `git-workflow` skill adds guidance for a multi-commit rebase — resolve
  commit 1 without relocating content, run the cheapest module-loading gate
  before each `rebase --continue`, and pull a later commit's fix forward
  when an intermediate tree cannot load
  (refs [#7231](https://github.com/bobmatnyc/trusty-tools/issues/7231))
- `test-driven-development` skill adds a Prop-Removal Checklist for a
  component that forwards `...rest` — grep call sites for the bare prop name
  and add a rendered-DOM assertion test, since `tsc` misses a removed prop
  that still compiles through the spread
  (refs [#7371](https://github.com/bobmatnyc/trusty-tools/issues/7371))
