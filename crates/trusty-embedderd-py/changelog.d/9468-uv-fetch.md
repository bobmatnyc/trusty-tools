Added

- The venv bootstrap now obtains `uv` with no user step: when `TRUSTY_UV_BIN`, `PATH` and the well-known install dirs have no `uv`, it downloads the pinned uv 0.12.23 release for `aarch64-apple-darwin` or `x86_64-unknown-linux-gnu`, checks the tarball against a SHA-256 digest embedded in the binary, and caches it at `<data>/py-embedder/uv/0.12.23/uv`. An installed `uv` always wins over the download. Set `TRUSTY_UV_FETCH=0` to disable the download ([#9468](https://github.com/bobmatnyc/trusty-tools/issues/9468)).
