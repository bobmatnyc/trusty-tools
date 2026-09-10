Added

- `tm doctor --fix` now repairs `hooks_build_tree_binary` instead of only
  reporting it. Every hook and `statusLine.command` whose executable lives in a
  Cargo build tree is REPOINTED at the installed `tm` binary, keeping its argv,
  so a corrupted `--pm-guard` entry comes back as a working `--pm-guard` entry
  rather than being removed and leaving PM enforcement offline until the next
  managed launch. The pass snapshots each file to
  `<name>.<YYYYMMDDTHHMMSSZ>.bak` before writing, is fail-closed on an
  unreadable, unparseable, or non-object settings file — reported, never backed
  up and never rewritten — and is idempotent: a repointed command names an
  installed binary, which the classifier rejects, so a second run writes
  nothing. It is the one repair that sweeps every settings file on the machine
  rather than the current project: the eight projects corrupted on 2026-09-09
  were none of them the cwd of the operator who ran `tm doctor`, which is why
  that repair had to be done by hand with `sed` (#7262).
