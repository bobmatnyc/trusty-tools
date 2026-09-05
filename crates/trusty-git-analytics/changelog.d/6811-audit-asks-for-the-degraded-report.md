Changed

- `tga audit` passes `--allow-degraded` to `trusty-review report`, so a run whose
  analyze lane assessed nothing still produces a bundle that states the fact
  rather than producing no bundle at all. The flag is offered only to a renderer
  at 0.35.0 or newer — tga resolves `trusty-review` from PATH, and an older copy
  rejects an unknown flag with a usage error (#6811).
