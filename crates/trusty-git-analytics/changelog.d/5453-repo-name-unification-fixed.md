Fixed

- `GitCollector` and the DD manifest derive a repository's name through one
  function (#5453). The collector had its own copy, which disagreed with the
  manifest's whenever a configured name was blank or whitespace — and a name
  the two sides spell differently joins zero rows rather than erroring, so the
  authorship section rendered confident zeroes instead of an error.
