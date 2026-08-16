# trusty-audit-ui — the auditor client's desktop shell

A Tauri 2 window over `trusty-audit`. Svelte 5 + Vite + TypeScript in `src/`,
the Rust host in `src-tauri/`.

## The one rule this crate exists under

DOC-68 §11 (`SPEC-AUDITPKG-11~draft`) fixes what this window may be: *"a view
over `Session::execute`, never a second place a capability can live."* Three
things follow, and none of them is negotiable:

- The Rust host calls `Session::execute` **in-process**, through a Tauri IPC
  command. Never a served HTTP endpoint, never a `taudit` subprocess.
- A new capability is a `session::Command` variant with a CLI arm. `Command` is
  `#[non_exhaustive]` and `trusty_audit::cli` matches it exhaustively, so a
  variant without a CLI invocation fails to compile. That match is the
  enforcement.
- `src-tauri/src/guided.rs` copies fields and chooses no behaviour. Wording is
  the front end's — the CLI phrases the same states as shell commands, this
  window phrases them as instructions — but the states themselves come from
  `Session::execute`.

## What phase 1 ships

One view: the guided status. Work-dir root, manifest state, per-tool install
status, and the next step. It adds no `Command` variant.

Repository selection, tool installation, the run view and the return package
are later phases of epic #5477 and still run from the `trusty-audit` command
line. Bundling, signing and notarisation are #5484 / #5481, which is why
`tauri.conf.json` sets `bundle.active: false` and `src-tauri/icons/icon.png` is
a placeholder rather than artwork.

## Building

```bash
pnpm install && pnpm build     # from this directory — writes dist/
cargo run -p trusty-audit-ui   # build.rs runs the two commands above for you
```

`dist/` is gitignored. `build.rs` builds it, and `SKIP_UI_BUILD=1` skips that
and embeds a placeholder page instead — for a host with no pnpm, and for CI,
which stubs the file itself. A binary built that way does not show the real
interface, and says so in its own window.

The shell reads `TRUSTY_AUDIT_WORKDIR` and the current directory to find the
engagement, the same two inputs `trusty-audit`'s `--work-dir` default resolves
from.

## Gates

```bash
cargo clippy -p trusty-audit-ui --all-targets -- -D warnings
cargo test -p trusty-audit-ui
pnpm build && pnpm check       # from this directory
```

The crate is a workspace member listed by full path in the root `Cargo.toml`
(the `crates/*` glob reaches one level, not three). It is absent from
`default-members` and `--exclude`d from the headless workspace jobs, because
Tauri links WebKit2GTK on Linux; the `audit-ui` CI job installs that toolchain
and verifies it properly.
