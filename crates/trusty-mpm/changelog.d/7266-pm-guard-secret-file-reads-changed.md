Changed
- `grep -o '^key_[a-z_]*' <file>` on a secret-bearing file is no longer carved
  out of `tm hook --pm-guard`. It was the one place a verb's FLAGS decided the
  verdict, and a second `-e` past the checked one — `grep -o -e 'pw=.*' -e '^K'
  .env` — reached the values. Hand such a file to the tool that needs it by
  absolute path (`-var-file`, `-state`, `--env-file`) instead (#7266).
