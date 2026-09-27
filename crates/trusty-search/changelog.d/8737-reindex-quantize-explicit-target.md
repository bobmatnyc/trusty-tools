Changed

- `reindex`, `quantize` and `index relocate` now refuse a target taken from
  `TRUSTY_INDEX` alone; pass `-i`/`--index` (or, for `reindex`, a PATH), or
  unset `TRUSTY_INDEX` and run from inside the project. Before, an exported
  `TRUSTY_INDEX` could re-point a live index at an unrelated PATH and
  overwrite its corpus (Refs [#8737](https://github.com/bobmatnyc/trusty-tools/issues/8737))
  - `reindex PATH` now reindexes the index registered at PATH, not the
    current directory's; PATH together with `-i`/`TRUSTY_INDEX` must name the
    same index, and a mismatch — or a daemon that cannot confirm the match —
    refuses before any reindex is sent
  - `reindex -i ID` now reindexes ID at its registered root instead of
    rebasing it onto the current directory
  - resolving a PATH to an index (`reindex PATH`, `index remove PATH`) now
    refuses when any registered index's status cannot be read, naming that
    index, instead of skipping it and matching another index at the same root
