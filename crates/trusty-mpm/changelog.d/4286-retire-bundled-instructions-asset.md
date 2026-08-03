Removed

- The bundled `crates/trusty-mpm/src/assets/instructions/INSTRUCTIONS.md`
  stub and the `FRAMEWORK_INSTRUCTIONS` constant/bundle-table entry that
  embedded it. This is the framework-owned launch-artifact stub, distinct
  from (and unrelated to) the project-level `.trusty-mpm/INSTRUCTIONS.md`
  override file retired by #4665. Its content never reached a live session:
  `tm install`/`tm launch` always overwrite the same on-disk path
  (`instructions/INSTRUCTIONS.md`) with the fully assembled system prompt in
  the same call, and the one legacy reader that treats it as optional
  (`build_instructions`) already discards its result for anything but a
  side-effect (#4286 split A).
