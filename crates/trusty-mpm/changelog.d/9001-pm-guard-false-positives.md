Fixed

- `tm hook --pm-guard` no longer refuses a SQL wildcard such as `` `db`.* `` in
  a `mysql -e` statement for naming `.*`: a `mysql`/`mariadb` `-e` statement is
  read as SQL. A `.env` named in the statement still refuses.
- `gh issue list --search ".env.*"` and `gh pr list --search …` are no longer
  refused: a search string reads no file. A `for` loop whose words reach only
  `echo` and that `--search` value is allowed too; any other use of the loop
  variable still refuses.
- A Python (or other non-shell) here-document that names no `gh` or `curl` and
  expands nothing is no longer refused as a `gh api` secret DELETE. An
  unquoted-delimiter body carrying a `$` is always judged, and an unreadable
  call that spells `curl` from variables (`$C$R -X DELETE …/secrets/X`) is
  now refused.
- A command with a `$'…'` quote and no tmux is no longer refused as an
  unresolvable tmux command. It is still refused, and the refusal now names the
  `$'…'` token it cannot decode.
- The read-only dispatch refusal now names the remedy for a path held in a
  shell variable: write the path out literally.
