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
- Two limits of the read rule are now written down where the rule is, rather
  than left to be rediscovered. A filename the command COMPUTES in-line —
  `cat $(printf '\056env')`, a `base64 -d` of the name — never appears as
  literal path text, so no word scan sees it; that class is unbounded, so it is
  documented beside the variable-indirection residual and pinned by a corpus
  row that must keep allowing. And `*.pem` covers a PUBLIC certificate as well
  as a private key, so `openssl x509 -in cert.pem -noout -text` denies — an
  accepted cost, since telling the two apart needs the file's bytes, which is
  the read being refused (#7266).
