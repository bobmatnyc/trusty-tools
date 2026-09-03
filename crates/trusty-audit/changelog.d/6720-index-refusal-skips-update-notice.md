Fixed

- A failed `trusty-search index` or `index add` now reports the real failure
  instead of the update-availability notice. `trusty-search` prints that notice
  on stderr ahead of every human-facing subcommand when crates.io has a newer
  version, and both refusal messages quoted the first non-empty stderr line — so
  on a machine one release behind, the notice became "the reason" and the actual
  diagnostic was discarded. One delivered audit run masked 60 of 61
  repositories this way, and because an indexing failure short-circuits the
  evidence-grounding pass, all 60 reports rendered "not assessed" with nothing
  recorded to diagnose. Stderr carrying only a notice is now reported as
  `no reason given (stderr carried only an update-availability notice)` rather
  than as a cause (#6720).
