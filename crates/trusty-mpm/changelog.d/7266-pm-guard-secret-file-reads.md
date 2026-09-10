Fixed

- `tm hook --pm-guard` now refuses any Bash command that NAMES a secret-bearing
  file, whatever the command does with it. `sed -n '38,46p' terraform.tfvars` —
  the command that printed a live ngrok authtoken into a transcript — denies,
  and so do `echo "$(cat .env)"`, `dd if=.env`, `tar cf - .env`, `xxd`,
  `base64`, `strings`, `php -r`, `deno eval`, `perl -ne`, `python -c`,
  `cp .env /tmp/x`, `mv .env x`, a shell `< .env` redirection and a process
  substitution. The rule keys on the FILE rather than on the verb: four earlier
  rounds enumerated reading verbs and each list was bypassed by a verb it did
  not name. A command this guard cannot lex denies too, because lexing is
  consulted only to grant the allowlist below (#7266).
- `ls`, `stat`, `file`, `test`/`[`, `rm` and `git add`/`rm`/`mv`/`status` are
  the only commands that may name such a file — they move, delete, stage or
  describe it, and none prints its bytes. The grant applies only when the file
  is a direct argument of one of those programs and the segment runs no nested
  command, so `ls $(cat .env)` still denies (#7266).
- The harness's native `Read` and `Grep` tools are covered on the same terms. A
  `Read` denies with or without an `offset`/`limit` range; a `Grep` denies on a
  `path` naming a secret-bearing file and on a directory `path` whose `glob`
  names a credential family (`*.env`, `*.netrc`, `.env*`, `id_*`). A directory
  search with no glob, and every ordinary extension glob (`*.toml`, `*.txt`,
  `*.log`, `*.csv`, `*.tf`, `*test*`), stay allowed (#7266).
- `cp`/`mv`/`install`/`rsync`/`ln` and shell output redirection refuse to
  reproduce a secret-bearing file under ANY destination name the read guard
  would print — `cp terraform.tfvars secrets.rs`, `cp .env ./notes.txt`,
  `cat .env > notes.md` — wherever it lands, not only inside a session worktree.
  A brace group is expanded first, so `cp {terraform.tfvars,notes.txt}` denies
  as the two operands bash will make of it. Copy first, read the copy after was
  the one-move bypass this closes (#7266).
- An ordinary source file whose name merely carries `token`, `secrets` or
  `credentials` stays readable (`tokens.css`, `credentials.rs`), and those three
  words used as words — `echo "no secrets here"`, `grep -rn credentials src/`,
  `npm install token-bucket` — are not treated as filenames at all. Every
  extension-typed family (`*.tfvars`, `.env*`, `*.pem`, `*.key`, `id_rsa*`,
  `*.p12`, `.netrc`, …) denies, `id_rsa` included as a bare word (#7266).
