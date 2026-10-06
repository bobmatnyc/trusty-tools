Fixed
- Tests that read repository content outside this crate resolve the checkout at runtime through `trusty_common::test_harness::test_repo_root()`, so a test binary built in one worktree under a shared `CARGO_TARGET_DIR` no longer reads another worktree's files (#9298).
