Changed
- Stack detection walks nested manifests instead of probing the project root
  alone: Cargo `[workspace] members`, plus a bounded depth-4 walk that finds any
  undeclared subdirectory project (`crates/*/ui`, `apps/*/web`). Dependency,
  build-output, and fixture trees, dot-directories, directory symlinks (the walk
  never leaves the project tree), and `.gitignore` directory names — bare or
  `**/name/` — are skipped. On trusty-tools the PM prompt's **Detected Project
  Stack** section now names the Svelte/TypeScript/Tauri and Python engineers
  beside `rust-engineer`, where it named `rust-engineer` alone before (#7781).
- Detection now reports why it stopped, in two separate flags.
  `detected_stack_engineers` returns a
  `StackDetection { engineers, truncated, depth_limited }` instead of a bare set.
  `truncated` means a RESOURCE cap (directories scanned, member cap) abandoned
  the walk, so the engineer list may be incomplete — it logs a WARN naming the
  cap, and the **Detected Project Stack** section tells the PM to treat the list
  as partial. `depth_limited` means manifests below the walk's declared depth
  went unprobed, which is the design's scope: it logs at DEBUG and renders one
  informational line. Keeping them apart matters because the depth bound trips on
  most real repositories, so a single combined flag was true nearly always and
  could not be failed closed on (#7781).
