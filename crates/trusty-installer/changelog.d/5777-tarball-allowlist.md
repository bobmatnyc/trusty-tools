Fixed
- Prebuilt tarball installs place only the crate's expected binaries (per the
  shared `installed_binaries` table). Release tarballs ship mode-0755
  `LICENSE`/`README.md`, which previously landed in the bin dir as if they
  were binaries (#5777).
