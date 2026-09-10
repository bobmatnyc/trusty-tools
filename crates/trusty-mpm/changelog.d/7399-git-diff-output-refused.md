Fixed

- `tm hook --pm-guard` now refuses a git `--output` option — `git diff
  --output=/tmp/o.diff` and the separated `git diff --output /tmp/o.diff` — with
  the reason a `>` redirection denies with. `--output` writes a file with no
  `>` in the command, so `has_file_write_redirection` never saw it and the PM
  could write past the ADR-0044 / ADR-0048 boundary. `--output` is a git DIFF
  option, so `log`, `show` and `format-patch` are covered by the same rule.
  The option is matched whole rather than by prefix, leaving
  `--output-indicator-new=+` alone, and the scan stops at `--`, where git parses
  pathspecs rather than options; `git diff --no-index a b` and every other
  read-only diff shape stay allowed (#7399).
