Fixed

- The confirmed launch now runs the one-shot chain instead of the bare sweep. The
  sweep reads `state/selected-repos.toml`, which only `clone` writes and nothing
  in the guided launch wrote — so a fresh recipient died at
  `NoRepositoriesSelected` right after being told "Everything is in place", and a
  recipient carrying a selection from an earlier `taudit clone` had those OLD
  repositories audited, reported as audited, and exited 0.
- `trusty-audit guided` prints the status card and exits again. The interactive
  decision now reads the parsed CLI rather than the `Command` it maps to, which
  cannot tell the named verb from a bare launch — under a pty that turned the
  documented spelling into an unbounded hang.
- The sweep question discards terminal typeahead before it is asked, so an
  operator double-tapping Enter to finish adding targets no longer starts hours
  of unattended work with the second newline. Enter still starts the sweep.
- A board-only registry reports `SelectRepositories` again. Any registered target
  counted as a repository, so `taudit add board jira:ACME` skipped repository
  selection, triggered a real multi-tool download, and reported `ReadyForRun`.
- A bare launch prints its status card without asking for an inference
  credential. The key is resolved once the operator confirms the sweep, not
  before the card — a config with a blank key made the read-only launch exit
  non-zero after three mismatched retypes, never printing anything.
- A launch backgrounded with `&` prints the card instead of stopping silently.
  Opening `/dev/tty` for write succeeds from a background process group, so the
  first read raised SIGTTIN; the terminal probe now also checks the foreground
  group.
