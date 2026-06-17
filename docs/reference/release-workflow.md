# Release Workflow Reference

Each crate is tagged independently using the pattern `<crate-name>-v<version>`,
e.g. `trusty-mcp-core-v0.2.0`. The version comes from the crate's `Cargo.toml`.

> **`tga` tag aliases (issue #1128):** `trusty-git-analytics` publishes under the
> short package name `tga`. The binary-release workflow accepts **both**
> `tga-v<version>` and `trusty-git-analytics-v<version>` tags — they resolve to
> the same build config and Homebrew formula. The parse step canonicalizes the
> `tga` prefix to `trusty-git-analytics` for config/path lookups while keeping
> changelog `--tag-pattern` matched to the literal pushed tag. Either form works;
> you no longer need to push a second `trusty-git-analytics-v<version>` tag after
> a `tga-v<version>` push.

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

### Docker end-to-end validation (release gate)

The Docker E2E harness (`.github/workflows/e2e-docker.yml`, driver `scripts/e2e-docker.sh`, docs `docker/e2e/README.md`) validates that published crates install cleanly from crates.io on a clean Linux image and pass minimal end-to-end scenarios.

**Release gate policy:**
- **On-demand** via `workflow_dispatch` (manual trigger): primary workflow; run anytime from the Actions UI.
- **As a release gate**: when a release batch spans **≥3 crates OR ≥3 PRs**, run the E2E validation manually before declaring the release complete. Smaller releases (1–2 crates, 1–2 PRs) may skip it and rely on standard CI.
- **Nightly schedule**: runs automatically at 02:00 UTC as a regression safety-net, catching published-artifact and crates.io breakage independent of releases.

The workflow is **not** triggered on every PR or every release; it is a deliberate gate. When needed, trigger it from **Actions → e2e-docker → Run workflow** or:
```bash
gh workflow run e2e-docker.yml
```

## Bundled Crates — Intentionally Skipped by `release.yml`

`release.yml` only builds standalone distributable binaries. Three crates are
**bundled** under the Single-Install Convention (epic #943) and have no
standalone binary target of their own — when their tags fire, the workflow
skips the build/release/homebrew-bump jobs and exits success:

| Bundled crate | Ships inside |
|---|---|
| `trusty-console` | `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-review`, `trusty-mpm` |
| `trusty-bm25-daemon` | `trusty-memory` |
| `trusty-embedderd` | `trusty-search` |

To add a new bundled crate in the future, add it to the `elif` block in the
"Resolve per-crate config" step of `.github/workflows/release.yml`.
