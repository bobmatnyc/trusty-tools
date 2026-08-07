Fixed

- `pm_guard` no longer always-allows a bare `MEMORY.md` write, matching the PM prompt's write prohibition ([#5086](https://github.com/bobmatnyc/trusty-tools/issues/5086))
  - `is_pm_orchestration_path` matched any path whose basename was literally `MEMORY.md`, so a top-level `MEMORY.md` landed in the HARD always-allow set that wins over the source-code rule. `core.md` (merged in [#5079](https://github.com/bobmatnyc/trusty-tools/pull/5079)) says never to write, update, or cite `MEMORY.md`, so the enforcement layer contradicted the prompt
  - the two legitimate targets — the palace index under `~/.trusty-tools/…/memory/MEMORY.md` and the retired `.trusty-mpm/MEMORY.md` override — already match on the `.trusty-tools` / `.trusty-mpm` path-component check, so dropping the basename match removes no real coverage. `TASK.md` is unchanged
