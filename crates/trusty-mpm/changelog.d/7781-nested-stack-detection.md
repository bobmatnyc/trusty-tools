Changed
- Stack detection walks nested manifests instead of probing the project root
  alone: Cargo `[workspace] members`, plus a bounded depth-4 walk that finds any
  undeclared subdirectory project (`crates/*/ui`, `apps/*/web`). Dependency,
  build-output, and fixture trees, dot-directories, and bare `.gitignore` names
  are skipped. On trusty-tools the PM prompt's **Detected Project Stack**
  section now names the Svelte/TypeScript/Tauri and Python engineers beside
  `rust-engineer`, where it named `rust-engineer` alone before (#7781).
