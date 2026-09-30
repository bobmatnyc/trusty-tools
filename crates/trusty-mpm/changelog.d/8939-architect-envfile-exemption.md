Changed

- The pm-guard secret-file rule lets the Architect's main thread run
  `tm env keys` and `tm env set` on a `.env`, `.env.local` or `.env.*` file
  inside its project directory or a tm-registered project root, with and
  without a bypass. Each such call writes an `architect-envfile` audit line
  with the path and key names only. A subagent, a PM, a piped or wrapped
  form, the `Write` tool and every command that prints a value stay denied.
