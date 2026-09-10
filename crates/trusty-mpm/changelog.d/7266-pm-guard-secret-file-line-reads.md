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
- The same guard now covers the harness's native `Grep` tool, which prints
  matching lines verbatim under `output_mode: "content"` — `Grep(pattern=".",
  path="terraform.tfvars")` dumped the file with no shell involved. A `path`
  naming a secret-bearing file denies, and so does a directory `path` whose
  `glob` names one (`*.tfvars`, `**/.env`); a directory search with no glob is
  ordinary work and stays allowed. `base64`, `basenc` and `diff` join the
  content-printing verb class, so `base64 .env` and `diff .env /dev/null` deny
  too (#7266).
- `cp`/`mv`/`install`/`rsync`/`ln` and shell output redirection now refuse to
  reproduce a secret-bearing file under a name the read guard prints freely —
  `cp terraform.tfvars secrets.rs`, `cat .env > notes.md` — for EVERY
  destination, not only one inside a session worktree. The read guard's
  carve-out for ordinary source and markup extensions was a one-move bypass
  until this half existed: copy from a main checkout, `$HOME` or `/tmp`, then
  read the copy. Renaming a genuine source file whose name carries
  `secrets`/`credentials`/`token` is untouched, as is a copy that keeps the
  source's own extension (#7266).
