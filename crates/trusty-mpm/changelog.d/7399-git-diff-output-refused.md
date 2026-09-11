Fixed

- `tm hook --pm-guard` now refuses the git invocations that write a file with
  no `>` in the command, which `has_file_write_redirection` never saw, letting
  the PM write past the ADR-0044 / ADR-0048 boundary. Each spelling was
  verified to write against git 2.54.0 (#7399):
  - `--output=<file>` and `--output <file>` on any subcommand — it is a git
    diff option, so `diff`, `log`, `show` and `format-patch` all take it
  - `git format-patch` with `-o <dir>`, `-o<dir>`,
    `--output-directory <dir>` or `--output-directory=<dir>`, and a bare
    `git format-patch`, which drops `NNNN-*.patch` into the working directory
  - `git archive` with `-o <file>`, `-o<file>` or `--output=<file>`
  - `git bundle create <file>`, whose output is a positional
- Read-only spellings stay allowed: `git format-patch --stdout`, an archive to
  stdout, `git bundle create -`, `git diff --no-index a b`, `git diff -o` (a
  revision there), `git clone -o` (the origin remote), and
  `--output-indicator-new=+` — every option is matched whole, never by prefix,
  and option scanning stops at `--`, where git parses pathspecs (#7399).
