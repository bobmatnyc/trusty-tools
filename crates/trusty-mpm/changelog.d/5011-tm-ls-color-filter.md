Added

- `tm f [pattern]` — find a session by name, filtering as you type
  - the list stays on screen and narrows per keystroke against the session NAME only; `[pattern]` seeds the filter box. Up/Down select, Enter opens the highlighted session through the numbered picker's own resume path, Backspace edits, Ctrl-U clears, Esc cancels
  - `tm ls <term>` is unchanged: still the one-shot, pipeable listing, still matching every visible column (id, name, project, state, task). `tm f` is the narrow lookup — a task description mentioning "api" no longer pads the answer to "which sessions are CALLED api-something"
  - piped output, `--json`, `--all`, or `TERM=dumb` never enter raw mode; they print the same static filtered table `tm ls` would. The gate is checked before raw mode is requested, so a script that pipes `tm f` cannot hang on a keyboard nobody is at
- `tm ls` colors the `ID` column, dimmed — it was the widest cell on the row and the only uncolored one, so the table read as a wall of undifferentiated UUID. `NUM`, `ID`, and `NAME` now carry three distinct hues, all suppressed on a pipe and under `NO_COLOR` as before

Fixed

- `tm ls` no longer staggers `NAME`/`TASK`/`CREATED` on rows whose state carries an annotation
  - the `STATE` column was a hardcoded 14 characters wide, but its rendered value is the state plus any annotation — `attached [stale-assets]` is 23 — so every annotated row pushed the three columns after it nine places right
  - the width is now measured across the rows actually being printed, floored at the historical 14 so an unannotated listing keeps its shape
