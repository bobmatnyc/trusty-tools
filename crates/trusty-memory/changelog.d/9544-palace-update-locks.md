Changed
- `palace_update` now takes the palace write lock. It waits for an in-flight `palace_rename` instead of recreating the old palace directory, and an old id relabels the renamed palace. A lock still busy at its bound is an error; the update is never saved unlocked (#9544).
