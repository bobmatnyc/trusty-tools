Added

- `tm hook --pm-guard` denies three more hard-floor classes for every session
  except the Architect's main thread: a network upload of local content
  (`curl` with `-d @file`, `-T` or a `-F` file field, `wget --post-file`,
  `scp` or `rsync` to a remote host, `nc` other than `-z`, and `ssh` fed from
  a pipe or a `<` file), a destructive disk tool (`diskutil`
  erase/partition/resize/delete and forced unmounts, `dd of=/dev/…`,
  `mkfs`, `newfs`, `fdisk`, `asr`, and destructive `hdiutil` verbs), and a
  force-push to the default branch (`--force`, `-f`, `--force-with-lease`,
  a `+ref` refspec, or `--mirror`; `main` and `master` when the default is
  unknown). A command the guard cannot parse that names one of these
  programs is denied. Plain downloads are not affected (#8878).
