Fixed

- `tm hook --pm-guard` now refuses a line-range or partial READ of a
  secret-bearing file, not only a `cp`/`mv` of one. `sed -n '38,46p'
  terraform.tfvars` — the command that printed a live ngrok authtoken into a
  transcript — plus `tail -n 3 .env.production`, `grep -n TOKEN app.tfvars.json`,
  every other content-printing verb (`cat`, `head`, `cut`, `awk`, `xxd`, …), an
  inline `python -c` program that opens such a file, a shell `< .env`
  redirection, and a `Read` tool call with or without an `offset`/`limit` range
  all deny, for the PM and every dispatched agent alike. A segment the guard
  cannot lex denies too when its words name such a file. The file classifier is
  the one `pm_guard_bash::secret_file_copy` already owns, read here at read
  scope so an ordinary source file whose name merely carries
  `token`/`secrets`/`credentials` (`tokens.css`, `credentials.rs`) stays
  readable, while every extension-typed family (`*.tfvars`, `.env*`, `*.pem`,
  `*.key`, `id_rsa*`, …) denies. The safe read documented in the issue,
  `grep -o '^key_[a-z_]*' <file>`, is still allowed (#7266).
