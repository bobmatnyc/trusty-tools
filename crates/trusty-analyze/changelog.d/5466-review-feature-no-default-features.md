Fixed

- **`--features review` pulled in trusty-review's entire default feature set, including a contributor-profile pipeline this crate never calls.** The `trusty-review` dependency carried no `default-features = false`, so enabling `review` transitively compiled `tga`, `rusqlite`, and a vendored libgit2 with no source-code trigger anywhere in trusty-analyze. It now takes `default-features = false` and gets only the `mcp` feature the `review` gate already names ([#5466](https://github.com/bobmatnyc/trusty-tools/issues/5466))
  - this had to land with the removal itself: trusty-review 0.16.0 deletes the `profile` feature, which would otherwise have broken `--features review` in a crate whose source nobody touched
