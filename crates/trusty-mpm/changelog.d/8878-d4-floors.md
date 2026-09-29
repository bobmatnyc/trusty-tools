Added

- `tm hook --pm-guard` denies three more hard-floor classes for every session
  except the Architect's main thread: a network upload of local content
  (`curl` with `-d @file`, `-T` or a `-F` file field, `wget --post-file`,
  `scp` or `rsync` to a remote host, `nc` other than `-z`, `socat`, a
  redirect into `/dev/tcp` or `/dev/udp`, `sftp` fed from a pipe, a file or
  `-b`, and `ssh` fed from a pipe, a `<` file, a here-document or a
  here-string), a destructive disk tool (`diskutil`
  erase/partition/resize/delete and forced `unmount`/`umount`,
  `dd of=/dev/…` after path normalization,
  `mkfs`, `newfs`, `fdisk`, `asr`, and destructive `hdiutil` verbs), and a
  force-push to the default branch (`--force`, `-f`, `--force-with-lease`,
  a `+ref` refspec, or `--mirror`, including any unique prefix of a long
  option, and any unknown long option; `main` and `master` when the default
  is unknown). A command the guard cannot parse that names one of these
  programs is denied. Plain downloads are not affected (#8878).
