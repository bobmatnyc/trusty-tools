Fixed

- The investigation budget now scales with repository size instead of being a
  flat per-repository cap, so coverage no longer collapses on the largest
  repositories — the ones a due-diligence reader needs most. A 3,000-file
  repository is read at 300 files rather than 40, and a repository small enough
  that the flat default already covered it resolves exactly as before. A cap an
  operator pinned through `--investigate-max-files`, the manifest, or the
  environment is used verbatim and never scaled (#6784).
