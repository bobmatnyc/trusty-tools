Fixed
- `build.rs` no longer watches `ui/dist/index.html`, its own output. `pnpm build` rewrote that file on every build-script run, so the next `cargo build` re-ran the script and recompiled the crate. The script now watches the UI source files that `pnpm build` reads.
