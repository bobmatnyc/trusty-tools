Fixed
- pm-guard's `gh api` secret-DELETE rule no longer reads a glob-only word that cannot match `gh`, `api` or `curl` — a Python subscript pair such as `d[k] x[0]` in a here-document body — as a rewritten program name. An expansion, a backtick, a brace and a glob that can spell the name still count (#9006).
- pm-guard's secret-file rule reads a `mysql`/`mariadb` `-e`/`--execute` statement as SQL, so `` `db`.* `` is no `.env.*` glob, unless the statement carries a backslash or a `system`, `pager` or `edit` client command that reaches a shell (#9006).
