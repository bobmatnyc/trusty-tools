Fixed
- The prebuilt tarball allowlist no longer drops unexpected executables
  silently: an extracted file that carries the execute bit, is not an obvious
  documentation file (LICENSE/README/CHANGELOG), and is missing from the
  shared `installed_binaries` table is now named in a warning before being
  skipped. Table drift used to produce a half-install with no trace at all
  (#5777, trusty-review round on PR #5778).
