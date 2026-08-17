Added

`index_id::identifies_same_path` — the one implementation of "do these two paths name the same directory tree?", comparing `(dev, ino)` when both exist and falling back to path equality otherwise. On case-insensitive APFS `canonicalize` preserves the spelling it was given rather than normalising it, so two cases of one directory compare unequal as strings; both registration guards now route through this rather than carrying separate copies of that rule. Filesystem-only, so a non-git tree compares exactly like a checkout.
