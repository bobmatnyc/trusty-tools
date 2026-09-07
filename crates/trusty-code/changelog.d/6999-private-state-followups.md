Fixed

- **Every `tcode` run now tightens a permissive `~/.trusty-code` (#6999).** The
  `0700` create-and-tighten was reachable only from `tcode paths import`, so the
  README's "tightened on the next run" was false for every other subcommand.
  `workstreams::default_data_dir` — the one resolver the workstream store,
  `serve`'s router, and `agent_loop::telemetry` all reach the private-state root
  through — now applies it, so no writer creates the directory at the process
  umask first.
- **`tcode run-task --help` describes the `.trusty-code` layout (#6999).** It
  still said the project root "must contain a `.claude/` directory" and that
  agents live in `.claude/agents/<name>.md`; both stopped being true in #5426.
  `tcode serve --help` carried the same sentence and is corrected too.
- **`tcode paths import` exits 1 when any entry was refused (#6999).** A symlink
  escape, an executable, a secret-bearing `settings.json`, or an
  already-existing target was printed as a `skip` line while the process still
  exited 0. `--dry-run` reports the same code the real run would, so the preview
  and the run cannot disagree.
