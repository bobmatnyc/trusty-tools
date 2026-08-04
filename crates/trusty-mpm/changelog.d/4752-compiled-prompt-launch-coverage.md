Fixed

- the compiled PM prompt is now refreshed at the spawn seam, so resume, guided-resume and crash-recovery launches no longer run a prompt that never reached `INSTRUCTIONS-COMPILED.md` (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - the compiled write is the last step of session preparation, so a failure can no longer skip the MCP content-pinning injectors that defend against the #3918/#3950 name-squatting class
