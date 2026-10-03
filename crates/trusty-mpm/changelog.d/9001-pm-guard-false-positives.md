Fixed

- `tm hook --pm-guard` no longer refuses `gh issue list --search ".env.*"` or
  `gh pr list --search …`: a search string reads no file. A `for` loop whose
  words reach only `echo` and that `--search` value is allowed too; any other
  use of the loop variable still refuses.
- An unreadable command that spells `curl` from variables or a glob
  (`$C$R -X DELETE …/secrets/X`) is now refused as a GitHub secret DELETE. The
  #8875 rule recognised only a literal `curl` there; this gap predates #9001.
- Two #9001 false positives are not fixed here and still refuse. A SQL
  wildcard such as `` `db`.* `` in a `mysql -e` statement (case 2) folds into
  #9006. A Python here-document refused as a secret DELETE (case 4) is split
  out to #9006 too.
- A command with a `$'…'` quote and no tmux is no longer refused as an
  unresolvable tmux command. It is still refused, and the refusal now names the
  `$'…'` token it cannot decode.
- The read-only dispatch refusal now names the remedy for a path held in a
  shell variable: write the path out literally.
