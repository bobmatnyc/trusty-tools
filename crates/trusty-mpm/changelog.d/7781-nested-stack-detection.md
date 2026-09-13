Changed
- Stack detection walks nested manifests instead of probing the project root
  alone: Cargo `[workspace] members`, plus a bounded depth-4 walk that finds any
  undeclared subdirectory project (`crates/*/ui`, `apps/*/web`). Dependency,
  build-output, and fixture trees, dot-directories, directory symlinks (the walk
  never leaves the project tree), and `.gitignore` directory names — bare or
  `**/name/` — are skipped. On trusty-tools the PM prompt's **Detected Project
  Stack** section now names the Svelte/TypeScript/Tauri and Python engineers
  beside `rust-engineer`, where it named `rust-engineer` alone before (#7781).
- Detection now reports whether it saw the whole tree. `detected_stack_engineers`
  returns a `StackDetection { engineers, truncated }` instead of a bare set, each
  scan bound (depth, directories scanned, member cap) logs a WARN naming itself,
  and the **Detected Project Stack** section states in one line when the list is
  partial — a short answer used to be indistinguishable from a repo that really
  has no nested stack (#7781).
