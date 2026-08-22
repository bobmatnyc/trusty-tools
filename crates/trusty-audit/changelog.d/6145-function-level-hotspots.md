Changed

- The complexity leg keeps the function it measured. `/complexity_hotspots`
  ranks chunks — one function each, with a name, a line range and a cyclomatic
  count — and the reader here parsed the file path and the count only, so
  collapsing chunks to files threw away the part that says WHERE in a 900-line
  file the complexity is. Each `inspect_priority` entry the complexity leg
  produces now carries a nested `hotspot = { function, start_line, end_line,
  cyclomatic }` table naming that file's worst function, and its `reason` line
  names it too. The file-level collapse is unchanged: one entry per file, the
  winning chunk's measurement recorded, the losing chunks still dropped.
  trusty-review reads the new key to point the investigation prompt at the
  function rather than the file (#6146).
- A manifest written before this parses exactly as it did — the key is absent,
  and an entry that has nothing else to say is still a bare path string. A
  daemon that ranks a chunk without locating it still ranks that chunk's file;
  the measurement is an addition to the entry, never a precondition for it.
