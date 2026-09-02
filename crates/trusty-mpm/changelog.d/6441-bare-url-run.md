Added

- `tm <github-url>` and `tm <owner>/<repo>` now provision and run a managed
  session in one command, instead of exiting with clap's unknown-subcommand
  error. The token routes through the same register → load → run chain
  `tm run <target>` already drives, so an already-registered repo refreshes and
  runs rather than duplicating a registration (#6441).
  - A leading token that is not repo-shaped — a subcommand typo such as
    `tm statuss` — still gets clap's usage error and the "did you mean?" hint.
  - Abbreviated subcommands (`tm sta` → `tm status`) keep resolving by prefix
    inference; they never fall through to the new catch-all.
  - Bare `tm` with no argument is unchanged.
