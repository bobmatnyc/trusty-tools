Fixed

- compiled PM system prompt now writes to a distinct `~/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md` instead of colliding with the bundled stub (closes [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - the compiled prompt is refreshed at every session launch, before `claude` is spawned; a failed write now aborts the launch rather than leaving a stale prompt on disk
  - `tm install` removes a stale pre-#4752 `framework/instructions/INSTRUCTIONS.md`, which no writer could refresh but the pipeline still read
