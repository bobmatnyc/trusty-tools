Added
- `palace_alias::PalaceAliasStore::rename_target` points a renamed palace's old id, and every alias of that old id, at the new id in one write. It fails, and leaves the file unchanged, when the alias file is corrupt. `PalaceAliasStore::remove_alias` removes one alias. `palace_alias::canonical_palace_id` names the palace id a request reaches through a live alias (#9544).
