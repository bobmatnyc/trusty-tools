# broken

The signing helper lives in `crates/trusty-installer/src/commands/gone.rs`,
which is exactly the shape issue #5147 names: a module file a 500-SLOC split
turned into a directory, with the citation left behind.

For contrast, this page cites itself at `docs/reference/broken.md`, which does
resolve — so a fixture that reported one finding rather than two would be
failing for the wrong reason.
