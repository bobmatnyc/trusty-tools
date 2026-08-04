Changed

- the compiled PM prompt is now written per project, to `<project>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`, and a failure to write it refuses the launch (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - removes the cross-project collision by construction: concurrent sessions no longer share one global file
  - the resume path writes it too, so resumed sessions can no longer run against a prompt that was never recorded
  - `tm install` no longer writes a compiled prompt — install has no project, so it has nothing to compile
  - a refused launch names the path and the remedy instead of surfacing a bare I/O error
