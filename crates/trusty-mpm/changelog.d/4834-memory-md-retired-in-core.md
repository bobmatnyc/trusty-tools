Changed

- the PM instruction package now carries a binding rule, in the non-overridable `core` section, that `MEMORY.md` (or any static memory-index file) is never a write target or a source — durable facts go to the palace instead, and `CLAUDE.md` is the only non-dynamic instruction source (closes [#4834](https://github.com/bobmatnyc/trusty-tools/issues/4834))
  - the PM Allowlist's "write a single non-source file" row no longer lists a bare memory file as an unbudgeted write
  - `sections/memory.md` carries a one-line pointer back to the new rule
