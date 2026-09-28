Fixed

- `tm hook --pm-guard` refuses more commands that print a credential into
  tool output (#8677). It now refuses `security -i` and `security -p`, which
  run commands read from stdin, as in
  `echo 'find-generic-password -w' | security -i`. It refuses `curl -v`,
  `--trace`, `--trace-ascii` and `--libcurl` when an argument carries a
  credential. It refuses a credential written to a redirect target chosen at
  run time (`> "$OUT"`), unless the command bound that name to `$(mktemp)`. It
  refuses a device path with extra `/`, `.` or `..` segments
  (`> /dev/./tty`), and it refuses any terminal device. It also refuses
  `gcloud secrets versions access`, `gh auth token`, `op read`,
  `aws configure get` for a secret or token key, and
  `aws configure export-credentials`, unless the value is captured, piped to a
  consumer, or written to a file.
