Fixed

- OAuth client-credential resolution/persistence (`client_store`) no longer
  silently falls back to the process's current working directory when the
  home directory cannot be determined; it now returns an explicit error
  instead of reading or writing credentials at a CWD-relative path (found in
  the 2026-08-19 self-audit).
