Fixed

- `audit::run_full_sweep` now reports every stage on the `ProgressBus` it is
  given, instead of discarding the parameter. Each of the eight stages emits a
  start event (naming the stage and its position in the run) and a
  completed/failed event under the new `Stage::Audit`, and the collection stage
  is handed that same bus so its per-repository events land there too. A `None`
  or disabled bus stays a no-op.
