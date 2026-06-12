# Release Workflow Reference

Each crate is tagged independently using the pattern `<crate-name>-v<version>`,
e.g. `trusty-mcp-core-v0.2.0`. The version comes from the crate's `Cargo.toml`.

## Release Steps

1. Bump the crate version in `crates/<name>/Cargo.toml`.
2. Update any dependent crates that pin that version.
3. Run `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`.
4. Commit the version bump.
5. Create the tag: `git tag <crate-name>-v<version>`.
6. Push the tag: `git push origin <crate-name>-v<version>`.
7. Publish: `cargo publish -p <crate-name>`.
   - **UI-embedding crates** (trusty-search, trusty-memory, trusty-analyze): prefix with `SKIP_UI_BUILD=1`:
     ```bash
     SKIP_UI_BUILD=1 cargo publish -p <crate-name>
     ```
     The committed `ui-dist/` bundle is already in the repo; without this flag, `build.rs` will attempt to invoke `pnpm` inside cargo's verification tarball, which fails because it tries to modify files outside `OUT_DIR`.
8. Build the release binary (if not already fresh): `cargo build --release -p <crate-name>`.
9. Install the binary locally with `cargo install --path crates/<dir> --locked`
   (for crates with binaries, e.g. trusty-search, trusty-mpm). This ensures the
   binary on PATH is always the version that was just released.

## macOS Code-Signing Critical Alert

🔴 **Never `cp target/release/<binary> ~/.cargo/bin/<binary>` on macOS.**

`cargo build` ad-hoc ("linker-signed") signs every release binary, and the
kernel's code-signing cache is keyed by the executable's `cdhash`. A plain
`cp` over an existing on-PATH binary can leave the kernel with a stale
cached identity, so the next exec is SIGKILL'd with
`EXC_CRASH / CODESIGNING — Taskgated Invalid Signature` **before any code
runs** — the process dies with `zsh: killed` and zero output, which looks
exactly like an OOM kill but is not. `cargo install` writes to a temp path
and renames atomically, which keeps the cache consistent. If you must copy
manually, follow it with `codesign --force --sign - ~/.cargo/bin/<binary>`
to regenerate the ad-hoc signature against the final file.

## Version Management

Every crate manages its own version independently in its own `Cargo.toml`.
The `[workspace.package]` table no longer carries a `version` field (see #343).
When publishing, bump only the crates that actually changed — do not cascade
version bumps to siblings with no functional changes.
