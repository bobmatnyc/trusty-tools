Added

- `core::claude_md_writer` — the single owner of `CLAUDE.md` section-override
  writes, the write-side counterpart to the reader-only
  `core::claude_md_sections` (refs [#4754](https://github.com/bobmatnyc/trusty-tools/issues/4754))
  - `write_section_override` is idempotent by construction: a repeat write
    replaces its block in place and collapses any pre-existing duplicate, so a
    section can never accumulate a stacked second copy
  - `core` is refused because the shipped package declares it `fixed` — the
    writer asks `CustomizationTier::permits`, it does not carry its own list
  - every refusal (protected section, empty body, a marker line inside the body,
    an unpaired marker in the host, an unreadable host) leaves the file
    byte-identical and the bundled section in force
  - `ensure_compiled_pointer` records where the composed PM prompt lands, using
    delimiters deliberately outside the `TRUSTY-MPM:` grammar so the reader sees
    no override and raises no diagnostic
