Fixed

- **`trusty-agents-ui`'s `build.rs` now builds the Svelte bundle**, so a clean
  clone compiles. Its `frontendDist` (`../dist`) is gitignored and nothing built
  it, so `tauri::generate_context!()` panicked with a bare "this path doesn't
  exist". This crate enables `tauri/custom-protocol` by default, which is what
  turns that check on for every Tauri crate in a workspace-wide build — it
  failed on its own `-p` build too. `build.rs` now runs `pnpm install` +
  `pnpm run build`, honours `SKIP_UI_BUILD=1`, and aborts loudly if pnpm is
  missing or the build produces no `index.html`
  ([#4699](https://github.com/bobmatnyc/trusty-tools/issues/4699))
