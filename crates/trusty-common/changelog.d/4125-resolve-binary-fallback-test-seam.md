Changed

- `bin_resolve::resolve_binary` now delegates to an internal
  `resolve_binary_in`, which takes the well-known-directory fallback list as a
  parameter. Behaviour is identical; the seam exists so the fallback branch —
  "find a binary the process `PATH` does not list", the branch a launchd-spawned
  daemon depends on — can be tested without mutating the process-global `PATH`
  (#4125)
