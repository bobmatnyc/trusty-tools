Security

- `tm daemon --sandbox` runs an isolated live-check daemon. It refuses to
  start, naming the variables but never their values, when any environment
  variable outside a closed allowlist is set (`HOME`, `PATH`,
  `TRUSTY_DATA_DIR_OVERRIDE`, `TRUSTY_MPM_ADDR`, `TMPDIR`, `TERM`, `LANG`, the
  `LC_*` locale categories, `USER`, `LOGNAME`, `SHELL`, `RUST_LOG`, and the
  `__CF_USER_TEXT_ENCODING` macOS sets itself), when `TRUSTY_DATA_DIR_OVERRIDE`
  is unset, or when `$HOME` is the account's real home as the password
  database records it. A sandbox daemon never starts the Telegram bot and
  never reads `.env.local`, the credential store or the Keychain through any
  path: credential resolution, the manager and activity-classifier inference
  stores, the `/activity` key-presence probe, the bug-report token and the
  `credential_reach` doctor row all answer "absent". `scripts/sandbox_daemon.sh`
  starts one under `env -i`, forwarding only allowlisted names; a hand-built
  sandbox daemon is no longer sanctioned.
