Added

- `tm env set <path> <KEY>` and `tm env keys <path>` edit a dotenv file
  without printing a value. `set` reads the value from stdin or from the login
  Keychain (`--from-keychain <service> --account <account>`) and refuses a
  `KEY=value` argument; it writes atomically with mode 0600. `keys` prints key
  names only, and prints nothing when any line is not a blank, a comment or an
  assignment. Both verbs run only in the bound Architect session, only on a
  `.env` or `.env.*` file inside the same scope the pm-guard allows, and only
  for a call the pm-guard exempted for the Architect's main thread: the guard
  writes a one-shot grant for that exact call, valid for one minute, and the
  verb refuses without it, so an Architect subagent is refused. They follow
  no symlink anywhere on the path and refuse a file with more than one hard
  link.
