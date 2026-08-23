Changed

- `PayloadStore` now has a genuinely lazy read path (#6200). New public
  `try_for_each_row(segment_filter, f)` decodes one row immediately before
  handing it to the callback and pulls the next entry from redb only if the
  callback continues, so an early `ControlFlow::Break` (or `Err`) reads and
  decodes nothing past it. `load_all` becomes a thin wrapper that collects the
  stream, so its behavior and signature are unchanged. The internal segment
  scan behind `lookup_id_for_uuid` and `list_segment` no longer pre-collects the
  whole segment into an owned `Vec`, and `lookup_id_for_uuid` now stops at the
  first uuid match instead of always scanning the rest of the segment. This
  removes the whole-table materialization a large palace incurred at startup
  hydration.
