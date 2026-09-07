Fixed

- An untracked target file written before the `provenance:` stamp existed is
  still adopted rather than skipped forever. Adoption's test was byte equality
  with the fresh composition, which the stamp broke for every pre-#4698 file;
  `agents::provenance::without_provenance_line` lets adoption accept the
  pre-stamp spelling too. The ledger now records the checksum of the bytes on
  disk rather than of the composition, so an adopted file matches its own row
  and the next deploy upgrades it into the stamped form (#4698).
