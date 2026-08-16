Changed
- Every install/upgrade write path now targets the canonical cargo bin dir
  (`$CARGO_HOME/bin`, falling back to `~/.cargo/bin`) instead of
  `~/.local/bin` (#5777, #4964 Phase 3): `download::default_install_dir()`
  delegates to the shared `canonical_bin_dir()`, `install.sh`'s default is
  `${CARGO_HOME:-$HOME/.cargo}/bin`, `tctl self-update` places into the
  canonical dir rather than `current_exe()`'s parent, and the deliberate
  `~/.local/bin` + `~/.cargo/bin` double-write in trusty-agents'
  `install-wrapper.sh` is deleted. Binaries no longer land in two directories
  with PATH order deciding which copy runs — the stale-daemon mechanism
  behind #2386. The unused `DEFAULT_INSTALL_DIR` const is removed.
