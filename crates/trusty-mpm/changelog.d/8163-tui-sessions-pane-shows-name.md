Fixed

- **`tm tui` sessions-pane rows show the session name.** Each row labelled
  itself with an 8-char UUID prefix while the Activity header, the kill and
  decommission prompts and `tm sessions ls` all showed the name; `short_id` is
  now only the fallback for a nameless row.
- **A task-less row shows its state word again.** The daemon emits
  `task: Some("")` rather than `None`, so the detail column's state-word
  fallback never fired and the column rendered blank.
