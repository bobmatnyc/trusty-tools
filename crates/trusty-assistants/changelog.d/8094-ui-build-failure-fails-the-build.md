Fixed
- A failed `pnpm install`/`pnpm build` now aborts `cargo build`/`cargo install` instead of silently embedding the "UI not built" placeholder page (#8094). `SKIP_UI_BUILD=1` and a host with no pnpm still embed the placeholder, and the `cargo:warning` names which of the two applied.
