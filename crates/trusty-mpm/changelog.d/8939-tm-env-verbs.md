Added

- `tm env set <path> <KEY>` and `tm env keys <path>` edit a dotenv file
  without printing a value. `set` reads the value from stdin or from the login
  Keychain (`--from-keychain <service> --account <account>`) and refuses a
  `KEY=value` argument; it writes atomically with mode 0600 and refuses a
  symlink. `keys` prints key names only, and prints nothing when any line is
  not a blank, a comment or an assignment. Both verbs run only in the bound
  Architect session.
