Fixed

- ADR-0044's main-checkout write boundary now judges every composition
  segment of a `Bash` command that names a write, not only the first one
  found across the whole command — a benign first write (`echo hi >
  notes.md`) used to hide a later segment's source write
  (`&& echo … > src/lib.rs`) from the rule entirely (closes [#8468](https://github.com/bobmatnyc/trusty-tools/issues/8468))
  - a `cd` segment's own trailing redirect is judged the same way as any other
    segment's
  - no segment's target is resolved against a directory a preceding `cd`
    moved to; every target resolves against the hook's own `cwd` (following
    `cd` moves to #8704)
