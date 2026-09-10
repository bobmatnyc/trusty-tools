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
- A GLOB that expands onto a secret-bearing file denies as the file itself does.
  `cat .en?`, `cat .e*`, `cat ./.*`, `cat id_rs?`, `cat *.p?m`, `cat id_[r]sa`
  and `sed -n '1,5p' terraform.tfvar?` each name a real file and each was
  allowed, because the word scan cut the command at the wildcard and screened
  the remainder. Ordinary globbing is untouched: `cat *.toml`, `ls *.json`,
  `rg foo src/*.rs` and `ls file?.txt` still allow (#7266).
- A name reassembled by quoting or escaping denies. `cat '.en''v'` and
  `cat .e\nv` both read `.env` and both were allowed, because the scan ran on
  raw text even when the command lexed cleanly. It reads the lexer's tokens
  now, and falls back to the raw scan only for a command no lexer can read,
  which is still the fail-closed arm (#7266).
- `git add` loses its exemption under `-p`/`--patch`, `-i`/`--interactive` and
  `-e`/`--edit`, which walk the file's diff through the transcript.
  `git add .env.example`, `git add -A` and `git add -u` are unaffected (#7266).
- A secret-bearing NAME written as a search PATTERN no longer denies:
  `grep -rn '\.env' docs/`, `rg 'id_rsa' --type md` and `grep -rn '\.pem'
  README.md` find where a file is referenced and print no byte of it. Only the
  first positional argument of `grep`/`egrep`/`fgrep`/`rg`/`ag`/`ack`/`git grep`
  is read that way, and only when no `-e`/`-f`/`--regexp`/`--file` supplied the
  pattern instead — so `grep -r SECRET .env`, `grep -e '\.env' .env` and
  `rg . .env` still deny on the file operand (#7266).
- An SSH PUBLIC key reads freely: `cat id_rsa.pub`, `cat ~/.ssh/id_rsa.pub` and
  `ssh-copy-id -i id_rsa.pub host` allow, while `cat id_rsa`, `cat id_rsa.pub.bak`
  and `cat id_rsa_credentials.pub` still deny. The exemption belongs to the read
  rule alone; copying a key file is unchanged (#7266).
