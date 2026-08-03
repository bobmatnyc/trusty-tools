Changed

- **`/api/v1/status` totals now cover cache-resident palaces only, and say so
  (issue #4637).** `total_drawers`, `total_vectors` and `total_kg_triples` are
  summed across the palaces resident in the open-handle cache rather than every
  palace on disk — that is what makes the endpoint answer at all at 5,794
  palaces. `palace_count` is unchanged and still reports the true on-disk
  total; a new `cached_palace_count` reports how many of those the three totals
  actually cover. The chat-surface `get_status` tool gained the same field.

- **`/api/v1/palaces` rows carry a `cached` flag (issue #4637).** When `cached`
  is `false` the row's `drawer_count` / `vector_count` / `kg_triple_count` /
  `wing_count` / `node_count` / `edge_count` / `community_count` are `0` because
  they are *unknown*, not because they are empty — the list route no longer
  opens a palace just to count it. Fetch `GET /api/v1/palaces/{id}` for live
  counts on a specific palace; the single-palace route still opens it and is
  unaffected. Both new fields are `#[serde(default)]`, so older clients that do
  not know them are unaffected.
