Fixed

- `window_id_of` read the window id only from a field with exactly three
  `:`-delimited components, so a tmux session name containing a literal `:`
  made the caller's own window unmatchable. It now reads the last component and
  requires it to look like a window id (`@N`), which leaves non-window input
  such as `a:b:c:d` parsing to nothing as before.
