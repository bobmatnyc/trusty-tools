Fixed

- `tm hook --pm-guard` no longer refuses a SQL wildcard such as `` `db`.* `` in
  a `mysql -e` statement for naming `.*`: a `mysql`/`mariadb` `-e` statement is
  read as SQL. A `.env` named in the statement still refuses.
- `gh issue list --search ".env.*"` and `gh pr list --search …` are no longer
  refused: a search string reads no file. A `for` loop whose words reach only
  `echo` and that `--search` value is allowed too; any other use of the loop
  variable still refuses.
- An unreadable command that spells `curl` from variables or a glob
  (`$C$R -X DELETE …/secrets/X`) is now refused as a GitHub secret DELETE. The
  #8875 rule recognised only a literal `curl` there; this gap predates #9001.
  The refusal of Python here-documents as a secret DELETE (#9001 case 4) is
  unchanged and is tracked in #9006.
- A command with a `$'…'` quote and no tmux is no longer refused as an
  unresolvable tmux command. It is still refused, and the refusal now names the
  `$'…'` token it cannot decode.
- The read-only dispatch refusal now names the remedy for a path held in a
  shell variable: write the path out literally.
