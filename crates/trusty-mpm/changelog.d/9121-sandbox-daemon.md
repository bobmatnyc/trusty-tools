Security

- `tm daemon --sandbox` runs an isolated live-check daemon. It refuses to
  start, naming the variables but never their values, when any `*_TOKEN` or
  `*_KEY` variable (or a `*_TOKEN_FILE`, `*_KEY_FILE`, `*_SECRET`,
  `*_PASSWORD` or registered credential name) is set, when
  `TRUSTY_DATA_DIR_OVERRIDE` is unset, or when `$HOME` is the account's real
  home as the password database records it. A sandbox daemon never starts the
  Telegram bot, and no credential read in it consults `.env.local`, the
  credential store or the Keychain, including the `credential_reach` doctor
  row. `scripts/sandbox_daemon.sh` starts one under `env -i` with an explicit
  allowlist; a hand-built sandbox daemon is no longer sanctioned.
