Added

- `tm doctor` gained an `rtk` check: `Ok` naming the resolved path (and `rtk --version` when the bounded probe answers), `Warn` with `install with `brew install rtk`; do not run `rtk init`, tm invokes rtk directly` when the binary is absent. `tm compress` shells out to rtk and falls back to a slower native compressor without it, silently, so an install that never pulled rtk in used to read exactly like one that did. Advisory only — a missing rtk never turns `tm doctor` red. The Homebrew formula now `depends_on "rtk"`, so only `cargo install` users install it themselves (#7311).
