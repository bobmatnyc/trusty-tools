Fixed

- A prebuilt `tctl install` or `tctl upgrade` of trusty-search on a host that
  once ran `cargo install trusty-embedderd-py` 0.1.x now takes cargo's record of
  the `trusty-embedderd-py` launcher over in `$CARGO_HOME/.crates.toml` and
  `.crates2.json`: the standalone record loses the launcher and one
  trusty-search record owns all three binaries. A later
  `cargo uninstall trusty-embedderd-py` no longer deletes the launcher, and a
  later `cargo install trusty-search --locked` no longer fails on a bin
  collision. Hosts without the standalone record, and hosts without cargo
  metadata, are not touched. Metadata the installer cannot parse or replace
  fails the install with the file name and the manual fix (#9974).
