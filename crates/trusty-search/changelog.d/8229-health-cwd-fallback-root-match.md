Fixed

- `search_health`: an unpinned session's working-directory fallback now confirms the derived index against the daemon's `root_path` list, the same check `serve`'s startup pin runs. Two checkouts that share a directory name each resolve to the index rooted at them (`resolved_from: "cwd_root_match"` when that index has another id), and an id served from a different tree reports `index_not_registered` naming that tree instead of `ok` (refs [#8229](https://github.com/bobmatnyc/trusty-tools/issues/8229))
