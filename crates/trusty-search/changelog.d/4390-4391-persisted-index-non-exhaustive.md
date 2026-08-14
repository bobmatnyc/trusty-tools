Breaking

- `PersistedIndex` is now `#[non_exhaustive]`, so future field additions stay non-breaking by construction. Construct it with `PersistedIndex::new(id, root_path)` (or `Default::default()`) and assign the fields you need — every field remains `pub`, so only the struct-literal syntax is withdrawn, not write access (#4390, #4391).
