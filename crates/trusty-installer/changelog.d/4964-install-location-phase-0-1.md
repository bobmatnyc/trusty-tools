Fixed

- `tctl upgrade` no longer installs every daemon member twice. On the prebuilt
  path the binary was already on disk when the daemon branch went on to call
  `upgrade_and_restart`, whose first step is `cargo install <crate> --locked` —
  a second copy, in a second directory, from one command. The comment saying
  that step was "a no-op if the binary is already current" was wrong: cargo
  skips only when its own `.crates2.json` records that exact version, and the
  prebuilt path writes no cargo metadata. Six of the seven stable-set members
  are daemons, so this fired on nearly every upgrade — and on a machine with no
  Rust toolchain it errored out after the new binary had already landed
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- `tctl upgrade` now actually restarts a daemon member. `upgrade_and_restart`
  restarts by calling `std::process::exit(1)` and letting launchd's `KeepAlive`
  respawn the process that just exited — correct for `trusty-search upgrade`,
  `trusty-memory upgrade`, and the two MCP `upgrade` tools, which all run inside
  the supervised daemon, and a guaranteed no-op for `tctl`, a terminal process
  launchd has never heard of. The supervision check evaluated `tctl`, returned
  false every time, and the manual-restart hint it produced was reported as
  success, so the daemon kept serving the old binary. Both daemon branches now
  bounce the member through the same launchd path `tctl restart` uses
  (port-guard, then `bootout`, then `bootstrap` — never `kickstart -k`)
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- `tctl install` passes the concrete path of the binary it just wrote to a
  member's `service install`, instead of a bare name resolved through `$PATH`.
  The spawned process bakes its own `current_exe()` into the launchd plist's
  `ProgramArguments[0]`, so a stale copy winning the `PATH` lookup persisted
  that stale path into launchd, which then respawned it at every boot with
  nothing to rewrite the plist
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- The component table's size column reads the binary this run just placed. It
  joined the binary name onto the cargo bin dir while the prebuilt path writes
  elsewhere, so it reported a stale copy's bytes, or zero
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- `install_all`'s install-directory fallback reads `CARGO_HOME`. It hardcoded
  `~/.cargo/bin` while the sibling fallback in `install_one` — same job, same
  file — did read it. Both, plus `tctl sign`'s `--dir` default,
  `tctl self-update`'s cargo-destination check, and `tctl upgrade`'s health-gate
  path, now share `trusty_common::bin_resolve::canonical_bin_dir`
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- Corrected the claim that `~/.local/bin` is preferred "to avoid cdhash issues
  on macOS". What keeps the cdhash cache consistent is the atomic rename in the
  download layer, which holds in any directory
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
