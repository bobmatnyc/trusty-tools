Fixed

- **`build.rs` now builds the Svelte bundle, so a clean clone compiles.**
  `tauri.conf.json` points `frontendDist` at `ui/dist`, which is gitignored and
  which nothing built — so `tauri::generate_context!()` panicked with a bare
  "this path doesn't exist" and took down `cargo test --workspace` and
  `cargo clippy --workspace` for every crate, not just this one. `build.rs` now
  runs `pnpm install` + `pnpm run build` in `ui/`, honours `SKIP_UI_BUILD=1`,
  and aborts loudly (naming the crate and the escape hatch) if pnpm is missing
  or the build produces no `index.html`
  ([#4699](https://github.com/bobmatnyc/trusty-tools/issues/4699))
