Removed

- `taudit clone --full` and `CloneOptions::shallow`. A full clone is now the
  only mode, so the flag had nothing left to select and the field was one
  assignment away from re-emptying the deliverable. Disk stays bounded by
  `--budget-gb`, which is unchanged: the same repository is 628 KiB shallow and
  1.2 MiB full, against a 20 GiB default.
