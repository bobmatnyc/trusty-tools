Fixed

- `PalaceStore::list_palaces` no longer reports a registry it cannot stat as an empty one ([#5532](https://github.com/bobmatnyc/trusty-tools/issues/5532))
  - The absence guard used `Path::exists`, which is `fs::metadata(..).is_ok()` and coerces every stat failure — `EACCES` from an unsearchable parent directory, `EIO`, `ELOOP` — to `false`. The function then returned `Ok(vec![])` without reaching `read_dir`, so the error propagation added in #5488 and #5526 never ran and the destructive callers (`purge_palaces`, `rebuild_palaces`, `merge_palaces`) reported a clean zero-palace run over data they never read.
  - It now uses `try_exists`, which keeps "absent" (`Ok(false)`) distinct from "cannot determine" (`Err`). A data root that does not exist yet — including one whose parents are missing, and a broken symlink — still returns an empty list, so first run is unchanged.
  - A macOS TCC denial is NOT this trigger: measured against real TCC-protected directories, TCC permits `stat` and denies only enumeration, so `read_dir` was always reached and the #5488/#5526 fixes fire as intended.
