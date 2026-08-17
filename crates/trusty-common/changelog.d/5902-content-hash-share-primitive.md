Added

- `memory_core::content_hash`: one content-addressed identity for memory bodies, with a versioned normalization contract (NFC, LF line endings, per-line trailing-whitespace trim, collapsed trailing newlines) so two machines that recorded the same fact produce the same digest. Normalization applies to hashing only — stored content is untouched (#5902).
- `Drawer::content_hash`, derived from `Drawer::content` at every point a drawer enters memory, plus `Drawer::set_content` and `Drawer::refresh_content_hash` to keep it in step. `Drawer::id` is unchanged and stays the vector-store, KG-subject, and slot-index key.
- `memory_core::share`: a palace-targeted JSONL export/import primitive keyed on the content hash. Importing the same export twice is a no-op, importing a superset adds only what is new, two machines' overlapping exports converge to one memory, and a merge keeps the earlier `created_at`. Exports exclude expired and Tier C drawers and carry no embedding vector; the room travels as its label so both machines mint the same UUIDv5 room id.
- `memory_core::share::supersede`: the `superseded_by` KG-triple writer, now shared with the dream cycle so both paths assert one edge shape and the #1713 guarantee (an original is only retirable once its provenance write is durable) lives in one place.
- `DrawerType::from_tag`, the inverse of `DrawerType::as_str`. `store::kg_redb`'s private `parse_drawer_type` delegates to it.

Notes

- The export path does NOT screen content for secrets — `filter::check_secret` is write-time only. Safe for a local file; a workflow that commits an export to a repository must re-scan first. See `docs/reference/shared-memory-identity.md`.
