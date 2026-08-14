Added

- `trusty-search` is now a pinned tool alongside `tga`, `trusty-analyze` and
  `trusty-review`. `[tools]` in the engagement config gains a required
  `trusty-search` key, and `trusty-audit install` fetches the binary into
  `work/tools/`.
- `trusty-audit run` sets `TRUSTY_SEARCH_BIN` on every `tga audit` child, so the
  audit's search preflight starts the engagement's pinned copy instead of falling
  through to a PATH lookup on a clean machine.
