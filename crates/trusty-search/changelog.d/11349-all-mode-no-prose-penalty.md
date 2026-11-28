Fixed

- A natural-language `search` at the default mode no longer multiplies prose
  and data files by Code's 0.1x and 0.2x penalty. The default mode already
  upgrades such a query to `all`, but `all` still took Code's penalty row, so
  a doc that ranked first in `mode: "text"` fell below every code chunk.
  Explicit `mode: "all"` now applies no file-type penalty either (Refs #11349).
