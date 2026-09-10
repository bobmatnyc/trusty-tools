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
- A `Grep` glob is now read as a PATTERN rather than as a literal filename, so
  `glob="*.env"`, `"*.netrc"`, `".env*"` and `"id_*"` deny instead of slipping
  past the denylist; previously only a glob spelled with a `*` in the same place
  as its denylist entry (`*.pem`) matched at all. A glob denies when it names a
  credential family — `*.toml`, `*.txt`, `*.log`, `*.csv`, `*.tf`, `*test*` and
  every other ordinary extension search stay allowed (#7266).
- A process substitution no longer hides a secret read. `diff <(cat .env)
  /dev/null` and `cat <(cat ~/.ssh/id_rsa)` lex into the tokens `<(cat` and
  `.env)`, which no filename rule could see; the wrapper now comes off before
  the basename match, and a segment carrying `<(`/`>(` beside a secret-shaped
  word is refused whatever verb sits inside it (#7266).
- `cp`/`mv`/`install`/`rsync`/`ln` and shell output redirection now refuse to
  reproduce a secret-bearing file under ANY destination name the read guard
  would print — `cp terraform.tfvars secrets.rs`, `cp .env ./notes.txt`,
  `mv terraform.tfvars vars.bin`, `cat .env > notes.md` — wherever it lands, not
  only inside a session worktree and no longer only for the 35 source and markup
  extensions. A brace group is expanded first, so `cp {terraform.tfvars,notes.txt}`
  denies as the two operands bash will make of it. Copy from a main checkout,
  `$HOME` or `/tmp` and then read the copy was the one-move bypass this closes.
  Renaming a genuine source file whose name carries
  `secrets`/`credentials`/`token`, a copy that keeps a secret-shaped name
  (`cp .env .env.bak`), a copy into a directory (`cp .env /tmp/`) and an
  `npm install token-bucket` are all untouched (#7266).
