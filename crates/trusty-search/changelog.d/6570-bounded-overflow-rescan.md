Fixed

- An FSEvents-overflow rescan no longer re-parses and re-commits the whole corpus when nothing changed. Every walked file is still read and fingerprinted, so full-tree coverage is unchanged, but a file whose content matches what the index already holds now skips the tree-sitter parse, the embed, and the redb commit. Observed before the fix: 22 overflow passes in 48h, each reporting 17,284 files reindexed and 183,348 chunks committed on an untouched tree (#6570).
  - The fingerprint cache is warmed from the durable corpus when the process has not populated it, so the first overflow after a daemon restart is cheap too.
  - The daemon's own colocated `.trusty-search/` storage joins the walker's skip list. It sits inside the watched root and is rewritten by every commit, and the watcher's per-event filter reads that list and never consults `.gitignore`.
  - The reconcile log line adds `files_unchanged`, which is what says whether an overflow found real work.
