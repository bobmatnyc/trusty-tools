Changed
- `tm ls`'s new-session picker now opens on the project of the session the
  cursor was on, renders each row as `owner/repo` (or `<basename> (local)` for a
  path-only registration) instead of the stored URL or path, and hides
  registrations nobody can work in — a temp-directory or `scratchpad` path, a
  path that is gone, or a directory that is not a git checkout (#7406).
