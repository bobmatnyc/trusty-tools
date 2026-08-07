Fixed

- `Bm25Stats` no longer carries `#[serde(default)]` on its fields. A daemon-side
  field rename decoded as `doc_count: 0`, which reads exactly like an empty
  palace; it now fails the decode.
