Fixed

- `tm fleet status`, `tm fleet init` and `tm env` without `--dir` now use the
  Architect recorded in `[supervisor] projects` of `~/.trusty-mpm/config.toml`,
  and fall back to `~/trusty-mpm-projects/architect` only when none is
  recorded. Two recorded Architects are an error that asks for `--dir`. The
  "incomplete" hint now names the checked directory:
  `tm fleet init --dir <dir>`.
