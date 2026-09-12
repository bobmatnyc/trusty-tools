Fixed

- The bundled `verification-before-completion` skill now teaches how to read a
  `128 + N` exit status beside its own "check exit code" step, so an agent stops
  reading its own `kill <pid>` as a crash. A dev server stopped on purpose was
  reported four times in one task as "failed with exit code 143"; 143 is
  `128 + 15`, a `SIGTERM` delivery, and the skill now maps 130 / 137 / 143 to
  their signals and says to report a deliberate stop as terminated by signal N
  rather than failed. The one `128 + N` value that is still a real failure —
  a 137 on a build or test run nobody killed, which is the OOM killer — is
  carved out explicitly so the rule cannot be used to wave off a genuine kill
  (#7561).
