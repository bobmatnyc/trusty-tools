# okf-extract

The trusty-okf extraction worker, binary `trusty-okf-xberg-worker`
(SPEC-OKF-01 §1.4, §2.16, Appendix B; slice S10b of #10746). trusty-okf's
`WorkerExtractor` (feature `extract-worker`) spawns it for every format but
UTF-8 text and Markdown, so trusty-okf never links xberg (B9).

- **Protocol.** Appendix B over stdin and stdout: `hello`, then one `ok` or
  `error` reply per `extract`, until `shutdown` or the end of stdin. stderr is
  diagnostic only.
- **Formats.** PDF, DOCX, XLSX and PPTX through `xberg = "=1.3.10"` with only
  the `pdf`, `office` and `excel` features: no OCR, no model, no URL, no
  network. Output is Markdown and byte-identical for identical input.
- **Limits.** The host enforces every limit and deadline. The worker reads
  bytes only, never a path, and writes no file.
- **Sandbox.** The host applies it (`bwrap` on Linux, `sandbox-exec` on
  macOS). The worker's `hello` reports what it observes (`bwrap`, `seatbelt`
  or `none`); the host uses that only as a cross-check.

## Where it builds

This directory is in the root `[workspace] exclude` (§3 (n) B) and is its own
workspace with its own `Cargo.lock` and `rust-toolchain.toml`. No workspace
crate depends on it; its tests depend on trusty-okf to drive the real binary
through the S10 client. `scripts/check_worker_dir_excluded.sh` guards the
exclusion.

```sh
cargo build --manifest-path workers/okf-extract/Cargo.toml --release
cargo test --manifest-path workers/okf-extract/Cargo.toml
# Reviewer check (criterion 104): no network or model-download feature.
cargo tree --manifest-path workers/okf-extract/Cargo.toml -e normal,build,features
```

xberg 1.3.10 declares `rust-version = "1.92"`, so the crate keeps the
workspace floor of 1.94.
