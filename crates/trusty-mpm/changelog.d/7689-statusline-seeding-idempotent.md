Fixed

- Statusline seeding is idempotent again: a second `ensure_statusline_entry_in`
  call on an unchanged settings file reports `Unchanged` and writes nothing.
  Where no `tm` binary is installed, command resolution degrades to the bare
  `tm statusline` literal, which the staleness predicate then claimed as its own
  stale pre-#1914 default — so every launch and every resume republished the
  file with byte-identical content. A repair that would write the identical
  command is now a no-op; a genuinely missing or divergent entry is still
  repointed. (#7689)
