Changed
- The crate is now published to crates.io: `publish = false` is removed and
  0.1.0 becomes the first upload (#4713). `trusty-code` depends on this crate
  from production code, and `cargo publish` refuses a crate whose non-dev
  dependency is unpublishable, so that line is what held `trusty-code` at 0.2.0
  on the registry while the tree moved to 0.4.0. The owner decided to publish
  rather than vendor the seam or freeze the registry copy.
- Added a `README.md` (with `readme = "README.md"` in the manifest) and an
  in-crate MIT `LICENSE`, matching how `trusty-code` and `trusty-mcp` ship.
  The license declaration itself is unchanged — `license.workspace = true`,
  which resolves to MIT.
