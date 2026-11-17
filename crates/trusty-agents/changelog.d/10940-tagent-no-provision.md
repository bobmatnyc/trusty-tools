Added

- `TAGENT_NO_PROVISION=1` stops `tagent` startup, `mcp-serve` included, from
  writing the bundled agents and workflows to `~/.trusty-agents` and from
  creating `~/trusty-agents/<instance>`
  ([#10940](https://github.com/bobmatnyc/trusty-tools/issues/10940)).
  `TRUSTY_SANDBOX=1` now implies it unless `TAGENT_NO_PROVISION=0`, and
  `--help`/`-h` never provisions. Without either variable, startup provisions
  as before.
- `scripts/sandbox_agents_daemon.sh` starts `tagent --api` with all state under
  one directory, a token file, stop and restart, and `--aws-profile NAME`, which
  passes the AWS profile file paths rather than credential values. Its selftest
  is `scripts/sandbox_agents_daemon_selftest.sh`.
