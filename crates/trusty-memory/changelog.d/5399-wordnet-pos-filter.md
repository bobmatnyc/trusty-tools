Fixed

- KG pattern extraction now takes the head noun of a noun phrase rather than the
  token nearest the marker, using part-of-speech membership from a vendored
  WordNet 3.1 projection. `match exhaustiveness is a hard requirement` yields
  `exhaustiveness --is-a--> requirement` instead of the adjective `hard`, and
  `the squash is an ancestor of origin/main` yields nothing instead of
  truncating a relation into the type `ancestor` — the residue #4678 could not
  reach (#5399).
- A phrase terminator hidden behind markdown emphasis no longer lets extraction
  run into the next sentence: `**MCP is a thin proxy.**` asserted
  `mcp --is-a--> sessions` because the raw token `proxy.**` ends in `*` and only
  its last character was checked.
