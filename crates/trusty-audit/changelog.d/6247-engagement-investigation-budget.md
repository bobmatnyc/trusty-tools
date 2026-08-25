Fixed
- The investigation budget an engagement declares now reaches the process that
  samples the files. `engagement.toml` gains a `[report]` section carrying
  `investigate_max_files` / `investigate_max_bytes`; the sweep resolves the
  budget ONCE from it (declared key > environment override > default, per key,
  with an absent byte budget derived from the file count) and hands that one
  value both to every `tga audit` child and to the grounding pass that records
  it. Before this the child read the environment inside its own spawn while the
  manifest was written from a second, later resolution — so a run could hand
  back a manifest declaring 240 files beside a report whose coverage section
  claimed 40.
