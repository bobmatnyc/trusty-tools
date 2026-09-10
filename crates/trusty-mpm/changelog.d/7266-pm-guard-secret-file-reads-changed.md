Changed
- `grep -o '^key_[a-z_]*' <file>` on a secret-bearing file is no longer carved
  out of `tm hook --pm-guard`. It was the one place a verb's FLAGS decided the
  verdict, and a second `-e` past the checked one — `grep -o -e 'pw=.*' -e '^K'
  .env` — reached the values (#7266).
- The deny message no longer offers `--env-file`/`-var-file`/`-state` as a way
  through. No such escape was ever implemented — `docker compose --env-file
  /repo/.env up` denied exactly as it does now — so the claim was removed
  rather than built: a list of reference flags is one more enumeration to
  bypass, which is the shape that failed four rounds running. The message names
  what actually works instead: run the tool so it picks the file up itself
  (`docker compose up` reads `./.env`), or ask the operator (#7266).
