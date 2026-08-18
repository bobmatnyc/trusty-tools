Changed

- `cli::registration::is_interactive` takes `&Cli` rather than `&Command`, and
  `guided_at_the_terminal` returns `Launch` rather than an `Outcome` so the
  front end owns when the credential is resolved.
- `README.md` no longer offers `TRUSTY_AUDIT_NO_LAUNCH=1` as a way to make the
  binary non-interactive. It is an `install.sh` variable deciding whether the
  installer starts the binary; the binary never reads it.
