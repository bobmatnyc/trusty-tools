Changed
- `PalaceRegistry::create_palace` refuses an id that is a live palace alias with `palace_alias::LiveAliasError`, instead of creating a palace that shadows the alias. It also fails when the alias file exists but cannot be read; a missing alias file still means no aliases. Ids that already own a palace, and aliases whose target is gone, create as before (#9544).
