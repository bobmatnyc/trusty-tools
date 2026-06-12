# Release Workflow — Full Reference

This page expands on the release convention mentioned in `CLAUDE.md`.

## Overview

Each crate is tagged independently using the pattern `<crate-name>-v<version>`, e.g. `trusty-mcp-core-v0.2.0`. The version comes from the crate's `Cargo.toml`. Every crate manages its own version independently — the `[workspace.package]` table no longer carries a version field (see #343).

When publishing, bump only the crates that actually changed — do not cascade version bumps to siblings with no functional changes.

## The Numbered Release Steps

These steps are canonical; see `CLAUDE.md` for the quick reference.

1. **Bump the crate version** in `crates/<name>/Cargo.toml`.
2. **Update dependent crates** that pin that version.
3. **Run tests:** `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`.
4. **Commit the version bump.**
5. **Create the tag:** `git tag <crate-name>-v<version>`.
6. **Push the tag:** `git push origin <crate-name>-v<version>`.
7. **Publish:** `cargo publish -p <crate-name>`.
   - **UI-embedding crates** (trusty-search, trusty-memory, trusty-analyze): prefix with `SKIP_UI_BUILD=1`:
     ```bash
     SKIP_UI_BUILD=1 cargo publish -p <crate-name>
     ```
     The committed `ui-dist/` bundle is already in the repo; without this flag, `build.rs` will attempt to invoke `pnpm` inside cargo's verification tarball, which fails because it tries to modify files outside `OUT_DIR`.
8. **Build the release binary** (if not already fresh): `cargo build --release -p <crate-name>`.
9. **Install the binary locally** with `cargo install --path crates/<dir> --locked` (for crates with binaries, e.g. trusty-search, trusty-mpm).

## Critical: macOS Cdhash and `cargo install`

**Never `cp target/release/<binary> ~/.cargo/bin/<binary>` on macOS.**

`cargo build` ad-hoc ("linker-signed") signs every release binary, and the kernel's code-signing cache is keyed by the executable's `cdhash`. A plain `cp` over an existing on-PATH binary can leave the kernel with a stale cached identity, so the next exec is SIGKILL'd with `EXC_CRASH / CODESIGNING — Taskgated Invalid Signature` **before any code runs** — the process dies with `zsh: killed` and zero output, which looks exactly like an OOM kill but is not.

**Solution:** `cargo install` writes to a temp path and renames atomically, which keeps the cache consistent. If you must copy manually, follow it with `codesign --force --sign - ~/.cargo/bin/<binary>` to regenerate the ad-hoc signature against the final file.

## Critical: macOS Full Disk Access Invalidation (Issue #873)

On macOS, every `cargo install` of a binary writes a NEW file at `~/.cargo/bin/<binary>` with a new **cdhash** (code-signing hash). macOS TCC keys the **Full Disk Access** grant by cdhash, so the previously-granted FDA no longer applies to the freshly-installed binary. The launchd daemon then cannot read indexes on `/Volumes/…` and warm-boot collapses from ~102 indexes to **indexes:2** (only non-external-volume indexes load).

### After every `cargo install trusty-search` (or any binary that accesses external/protected volumes as a launchd daemon), re-grant FDA:

1. Open **System Settings → Privacy & Security → Full Disk Access**.
2. Remove `~/.cargo/bin/trusty-search` from the list.
3. Re-add it (`+` button, navigate to `~/.cargo/bin/trusty-search`).
4. Restart the daemon:
   ```bash
   launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
   ```

### Symptom

`trusty-search status` shows `indexes:2` (or very few) immediately after a reinstall, with warm-boot logs showing `skipped: blocked-volume` or `tcc=57`. This is NOT data loss — all on-disk indexes are intact.

The daemon now detects this automatically: when the loaded count drops below 80% of the prior-known count, `GET /health` returns `warm_boot_degraded: true` and the daemon logs an error with the actionable FDA re-grant hint.

## Connection-Safe Daemon Restart

As of trusty-common 0.10.0, all three HTTP daemons (trusty-memory, trusty-search, trusty-analyze) implement graceful shutdown: they drain in-flight requests before exiting when they receive SIGTERM. The `mcp_bridge` binary reconnects automatically with exponential backoff when the daemon restarts.

**Use `launchctl bootout` (SIGTERM), not `launchctl kickstart -k` (SIGKILL):**

```bash
# Graceful stop → install → restart
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
cargo install --path crates/<dir> --locked
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
# IMPORTANT on macOS: re-grant Full Disk Access after cargo install (see above)
```

Prefer restarting between Claude Code sessions. See the cargo-publish skill (`.claude/skills/cargo-publish/SKILL.md`) for the full restart convention.

## Why Per-Crate Tagging?

The former repos — `trusty-common`, `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-git-analytics`, `trusty-mpm`, and `open-mpm` — each had independent release cycles and version numbers. Consolidating them into a monorepo does not require synchronizing releases. Each crate that publishes to crates.io maintains its own version and release tag independently. This allows:

- Releasing only the crates that changed
- No unnecessary version bumps for unmodified crates
- Clear traceability of which version fixed which issue
- Independent semver policies per crate

## Workspace Dependency Sharing

The root `Cargo.toml` maintains `[workspace.dependencies]` for all shared external crates and a `[patch.crates-io]` block that ensures in-tree crates are preferred during local builds even if a published version exists. This means:

- The path dep in `[workspace.dependencies]` coexists with the version field, so `cargo publish` sees the version and uploads correctly.
- The `[patch.crates-io]` block in the root `Cargo.toml` ensures the in-tree crates are preferred during local builds even if a published version exists on crates.io.

This eliminates the need for `[patch]` tables in individual crates and keeps version management centralized while still publishing each crate independently.
