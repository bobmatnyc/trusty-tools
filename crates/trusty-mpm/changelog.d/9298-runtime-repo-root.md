Fixed
- `tm generate capabilities` probes the documentation index in `references/framework.md` against the checkout the command runs in, not the checkout that built `tm`. An installed `tm` built from a reclaimed worktree no longer renders every repo-only doc as missing (#9298).
- Tests that read repository content resolve the checkout at runtime through `trusty_common::test_harness::test_repo_root()`, so a test binary built in one worktree under a shared `CARGO_TARGET_DIR` no longer reads another worktree's files (#9298).
