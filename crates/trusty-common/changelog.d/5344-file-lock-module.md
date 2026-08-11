Added

- `file_lock::with_exclusive_lock` / `file_lock::lock_path` — the cross-process
  advisory lock `json_rmw` already owned, extracted so non-JSON whole-file
  read-modify-write cycles can share it
  ([#5344](https://github.com/bobmatnyc/trusty-tools/issues/5344)).
  trusty-search's `indexes.toml` has its own TOML loader and fail-closed parse
  contract, so it cannot route through `json_rmw::update`, but it has the
  identical lost-update failure. `json_rmw` now calls the extracted primitive
  rather than owning it, and `json_rmw::lock_path` is a re-export — no behaviour
  change for existing callers.
