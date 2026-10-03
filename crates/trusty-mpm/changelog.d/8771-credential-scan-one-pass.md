Fixed

- The credential-print check in `tm hook --pm-guard` no longer rescans a
  command that binds a credential to a variable unless the first pass judged
  some text before that binding could reach it (a loop, a function, a later
  substitution). A command such as `T=$(gcloud auth print-access-token); …`
  is scanned once again, and each pass that does run gets its own work
  budget, so a large command is refused at the same size as before the
  rescan was added. A command that would need more than four passes is
  refused.
