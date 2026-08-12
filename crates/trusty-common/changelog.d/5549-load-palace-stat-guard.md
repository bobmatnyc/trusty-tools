Fixed

- `PalaceStore::load_palace` no longer reports a palace it cannot stat as one that is not there. Its absence guard was `Path::exists`, which is `fs::metadata(..).is_ok()` and so collapses a permission denial into `NotFound` — the one error `list_palaces` treats as benign and skips, so a palace whose permissions changed mid-walk dropped out of the listing the destructive passes act on. A denied or otherwise undeterminable probe now propagates an `Io` error naming `palace.json`; genuine absence still returns `NotFound` and still skips silently (#5549, ADR-0045).
