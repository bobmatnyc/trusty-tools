Added
- `GET /sessions/{id}/output` (and `mpm.sessions.output` / `mpm.sessions.pane`)
  reports `input_box`: `empty`, `suggestion` or `typed`, or `null` when the box
  could not be read. A `suggestion` is Claude Code's dim next-prompt text,
  which a plain capture shows exactly like a typed draft. `tm sessions output`
  prints it as `[input box: …]` on stderr, so stdout stays the pane text.
