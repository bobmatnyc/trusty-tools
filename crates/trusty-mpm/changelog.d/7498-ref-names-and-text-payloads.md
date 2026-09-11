Fixed

- `tm hook --pm-guard` no longer refuses a git REF argument or a human-readable
  text payload for naming a secret-bearing file. `git checkout -b
  feat/7526-secrets-manager-agent`, `git switch -c feat/7527-tm-secrets-skill`,
  `git branch [-m] <ref>`, `git push origin <ref>` and a `gh issue comment
  --body`/`--title`/`--message`/`--note` whose prose names a `.env` file all
  allow. Both positions are identified by POSITION, never by verb: a ref
  argument withdraws only the directory-prefix proxy, so `git checkout -b .env`
  and `git push origin id_rsa` still deny, and a payload is matched by exact
  long-flag spelling, so `gh issue comment 1 --body-file .env` and `git commit
  -F .env` still deny (#7498).
