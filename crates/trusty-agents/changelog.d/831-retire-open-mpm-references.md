Fixed

- Retired stale `open-mpm` references left over from the crate's #831 rename
  to `trusty-agents` (dir `crates/trusty-agents`, binary `tagent`). The REPL
  banner rendered `Open MPM v<version>` instead of `trusty-agents v<version>`;
  `Makefile`, `scripts/install-wrapper.sh`, and the renamed
  `scripts/tagent-wrapper.sh` (was `open-mpm-wrapper.sh`) pointed at a
  `target/release/open-mpm` binary that no longer exists and env vars
  (`OMPM_URL`, `VITE_OMPM_PORT`) the UI stopped reading; the integration/
  regression test harnesses (`tests/harness/*.sh`, `tests/integration/*.sh`)
  built `--bin open-mpm` and read `.open-mpm/tasks/`, both dead paths.
  Genuine `OPEN_MPM_*` back-compat env-var fallbacks (`env_compat.rs` and its
  ~30 call sites) and the `.open-mpm` legacy config-dir migration in `lib.rs`
  are unchanged — they still read the deprecated names for existing users.
