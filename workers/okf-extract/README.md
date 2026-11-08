# okf-extract

Reserved for the trusty-okf extraction worker (SPEC-OKF-01 §2.16, §3 (n),
decided B; issue #10745).

The worker depends on xberg, which needs Rust 1.98. The workspace MSRV is 1.94
(ADR-0029), so this directory is listed in the root `[workspace] exclude`. It
will carry its own `Cargo.toml`, `Cargo.lock` and `rust-toolchain.toml`
(1.98) and builds with `--manifest-path` or from inside this directory. It is
not a workspace member and no workspace crate depends on it.

`scripts/check_worker_dir_excluded.sh` guards the exclusion. No code lives here
yet.
