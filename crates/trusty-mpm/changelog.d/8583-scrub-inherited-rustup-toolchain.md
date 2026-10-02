Fixed

- A tm-managed Claude session no longer inherits `RUSTUP_TOOLCHAIN` from the
  `tm` that launched it. A `tm` run through a mise shim carried the toolchain
  mise resolved for its own directory, and rustup ranks that variable above a
  project's `rust-toolchain.toml`, so agents ran clippy and tests on the wrong
  toolchain. Every launch path now removes it, as the daemon's already did, and
  the session's project decides the toolchain. (Refs #8583)
