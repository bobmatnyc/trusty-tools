Fixed

- `tm hook --pm-guard` refuses more commands that print a credential into
  tool output (#8677). It now refuses `security -i` and `security -p`, which
  run commands read from stdin, as in
  `echo 'find-generic-password -w' | security -i`. It refuses `curl -v`,
  `--trace`, `--trace-ascii` and `--libcurl` when an argument carries a
  credential. It refuses a credential written to a redirect target chosen at
  run time (`> "$OUT"`), including a name bound to `$(mktemp)`. It
  refuses a device path with extra `/`, `.` or `..` segments
  (`> /dev/./tty`), and it refuses any terminal device. It also refuses
  `gcloud secrets versions access`, `gh auth token`, `op read`,
  `aws configure get` for a secret or token key, and
  `aws configure export-credentials`, unless the value is captured, piped to a
  consumer, or written to a file.
- `tm hook --pm-guard` refuses the credential-print bypasses the #8677
  review found (#8677). It refuses `security -i` behind any wrapper, such as
  `arch -arm64`, unless the program only reads the word, as `grep` does. It
  reads a device path such as `/DEV/stdout` in any letter case. It refuses a
  relative target that may name a device, a relative target after `cd`,
  `pushd` or `popd`, and a `~+` or `~-` target. It reads a `tee` or `dd of=`
  output file like a redirect target, so `tee "$OUT"` refuses. It refuses a
  `gh`, `op`, `aws` or `gcloud` subcommand chosen at run time, also after a
  global flag and its value (`gh -R owner/repo auth …`) or inside a flag word
  (`--hostname=$(…)`). To keep a credential in a file, write it to a literal
  `~/…` path under `umask 077`; a variable target such as `$HOME/…` refuses,
  and the deny message now says so.
