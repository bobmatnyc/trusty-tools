Fixed

- `tm ls` no longer labels a session with two or more attached tmux clients
  `(active)` instead of `(attached)`. `#{session_attached}` is a client count,
  not a boolean, and the row parser compared it to the literal `"1"` — so the
  session the operator was actually sitting in (two terminals on it) read as
  detached while every single-client session read correctly.
