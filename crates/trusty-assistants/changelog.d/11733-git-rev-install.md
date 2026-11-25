Fixed

- `ta --version` from a `cargo install --git … --rev <sha>` build no longer reports `-dirty` on a clean commit: the build script ignores the untracked `.cargo-ok` marker cargo writes into its git checkout, while any other modified or untracked file still marks the build dirty ([#11733](https://github.com/bobmatnyc/trusty-tools/issues/11733))
- `scripts/install-trusty-assistants-signed.sh` runs its `cargo install` with `CARGO_NET_GIT_FETCH_WITH_CLI=true`, so a host whose `git` authenticates but whose ssh-agent holds no identity can fetch the rev, and with a new `--target-dir` instead of an inherited `CARGO_TARGET_DIR`, so a reinstall at a new rev no longer keeps the previous rev's commit stamp ([#11733](https://github.com/bobmatnyc/trusty-tools/issues/11733))
