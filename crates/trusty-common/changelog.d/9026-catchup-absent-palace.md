Fixed
- The catch-up digest treats a `memory_list` not-found refusal (a palace that was never created) as an empty palace. It no longer writes "could not reach trusty-memory" to stderr on every run for such a project, or renders the memory section as unreachable. Any other refusal still reports the daemon unreachable (#9026).
