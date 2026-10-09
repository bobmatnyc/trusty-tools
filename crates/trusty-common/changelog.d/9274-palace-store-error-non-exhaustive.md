Changed
- `PalaceStoreError` is now `#[non_exhaustive]`; a `match` on it outside trusty-common needs a wildcard arm. `PalaceStore::save_palace` keeps an existing `format_version` mirror and refuses to rewrite a `palace.json` that names a newer format (#9274).
