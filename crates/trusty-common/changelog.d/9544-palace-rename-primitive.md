Added
- `PalaceRegistry::rename_palace` moves a palace to a new id: it writes `old -> new` into the alias map, moves the directory, and rewrites `palace.json`. It refuses a missing or aliased source, an invalid target, a target that is another palace's alias, and a non-empty target; with `replace_empty` an empty target goes to `<root>/.trash/<new>-replaced-<UTC>/`. A referenced handle makes it answer `Busy` with nothing changed, a failed move rolls the alias keys back, and a re-run finishes a rename a crash left partway (#9544).
- `PalaceAliasStore::rename_target` now returns an `AliasUndo` naming every alias key it changed, and `PalaceAliasStore::undo` restores exactly those keys (#9544).
- `memory_core::palace_emptiness::check_palace_empty` is the shared "this palace is empty" check: no drawers, no unabsorbed legacy data, and no chat sessions (#9544).
- `json_rmw::update_blank_as_absent` reads a whitespace-only document as absent (#9544).
