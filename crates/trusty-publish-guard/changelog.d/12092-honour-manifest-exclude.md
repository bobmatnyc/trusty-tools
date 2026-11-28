Fixed

- Version parity now compares only the `src/` files `cargo package` ships, honouring the manifest's `exclude`/`include`, so excluded test modules are no longer reported as "added" drift (closes [#12092](https://github.com/bobmatnyc/trusty-tools/issues/12092))
