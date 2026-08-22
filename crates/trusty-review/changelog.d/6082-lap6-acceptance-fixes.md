Fixed

- A component carrying whitespace or parentheses is read as a topic phrase, not
  a file. `trusty-common (memory_core/store)` slipped through as a path because
  its parenthesised half carries a slash, and the narrative citing it rendered as
  a numbered AMBER finding whose entire evidence was the file's own path. (Refs
  #6082)
- A synthesis narrative the finding bands refuse is now recorded under Synthesis
  Status with the component that matched nothing, instead of disappearing without
  a line. (Refs #6082)
- The reachability guard now runs over the verified investigation's own prose,
  before that prose is copied onto the model and merged over the synthesis
  narrative. A RED finding's business impact reached the page stating "a remote
  code execution risk" about a loopback-bound daemon because only the synthesis
  copy was guarded and the merge discarded it. (Refs #6082)
- A finding whose own text says "local-socket" or "unix socket" is read as
  loopback-scoped, the same as one that says "localhost". (Refs #6082)
