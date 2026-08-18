Fixed
- Stopped four `spawn_disclaim` doc comments from linking into the macOS-gated
  `macos` submodule (`resolve_disclaim_fn`, `spawn_stderr_piped_disclaimed`,
  `spawn_stdout_piped_disclaimed`, `wait_for`), which rustdoc cannot resolve on
  Linux. docs.rs builds on Linux once per release and never rebuilds.
