# Fact-check reconciliation

**Status:** CURRENT — verified reference.
**Date:** 2026-07-31
**Provenance:** independent verification pass resolving factual divergences between
research tracks A and B.

1. **Rust version pin — TRACK B CORRECT.** Root `Cargo.toml:80` sets
   `rust-version = "1.91"` (real MSRV floor, forced by `aws-smithy-*` transitive deps).
   MSRV CI jobs use `dtolnay/rust-toolchain@1.91` (`ci.yml:713`,
   `capabilities-drift.yml:32`, `sld-lint.yml:32`, `version-parity.yml:39`,
   `release.yml:864,1205`). Caveat: most regular CI jobs use `@stable`
   (`ci.yml:108,162,295,757,849,1062`), and
   `crates/trusty-git-analytics/rust-toolchain.toml:2` pins `channel = "stable"`.
   **No `.mise.toml`/`mise.toml` exists anywhere in the repo** — this repo does not
   manage Rust via mise at all.

2. **`tart exec` — v2.32.1.** `tart exec [-i] [-t] <name> <command>`; `-i` attaches
   stdin, `-t` allocates PTY. **No `--env` flag. No file-transfer subcommand** —
   transfer must go via stdin, `--dir` mounts, or SSH. `tart run --dir=<[name:]path[:options]>`
   supports mounts; read-only confirmed via `--dir="~/src:ro"`.

3. **Prior-art doc exists.**
   `docs/trusty-installer/research/02-design/10-isolation-testing-harness.md`, Status
   `Accepted (owner-approved)` (line 3), owner-approved 2026-06-09 (line 649). Already
   mandates: tart as VM tech (§3.2); vanilla `$HOME` is NOT isolation on macOS because
   launchd is per-uid (§3.1); `cargo install` only, never `cp` binaries into PATH for
   cdhash safety (§3.4); `released` vs `worktree` BOM modes (§7.1); JSON-only assertion
   oracle; shell for VM lifecycle. §7.3 verbatim: *"The scenario driver + assertions
   live as an `#[ignore]`-tagged integration test... It belongs in
   `crates/trusty-controller/tests/isolation_harness.rs`"*. **`trusty-controller` was
   renamed `trusty-installer` per ADR-0013** (Accepted 2026-06-26, supersedes ADR-0006)
   — all path references stale. No ADR supersedes DOC-10. Implementation checklist
   entirely unchecked — nothing was ever built.

4. **`tctl install` scope trap — TRACK B MOSTLY CORRECT.**
   `crates/trusty-installer/src/commands/install.rs`, `install_one()` (lines 757–862):
   prebuilt-tarball-first, then fallback to `cargo install <crate> --locked` against
   crates.io. **No `--path` code path exists.** Confirmed: `tctl install` cannot install
   from a worktree and must be avoided in branch/local-source patterns. ADR-0021 is
   Status **Proposed**, not Accepted, and concerns banning `cargo install --path` into
   `~/.cargo/bin`; Track B's citation was a reasonable inference, not a direct quote.
   Correct verification subcommands (from `cli.rs`): `up`, `install`, `update`,
   `ensure` (`--wait`), `stack health`, `stack doctor`, `doctor --self-check`, `status`,
   `version`, `self-update`, `sign`.

5. **`build.rs` source-tree writes — TRACK B CORRECT on count.** Five files write into
   `$CARGO_MANIFEST_DIR` not `$OUT_DIR`: `trusty-agents/build.rs` (63,139,146),
   `trusty-memory/build.rs` (29-31,130,136), `trusty-console/build.rs` (25-27,126,132),
   `trusty-analyze/build.rs` (25-27,126,132), `trusty-search/build.rs`
   (31-35,64-66,102). `trusty-search/build.rs:64-66` runs
   `Command::new("make").arg("release-prep").current_dir(&crate_root)`. **NOTE: the
   adversarial review substantially qualified this — see `devils-advocate-review.md`.**

6. **Expected binaries (authoritative, from `[[bin]]` targets):**
   trusty-installer → `trusty-installer`,`tctl`; trusty-search → `trusty-search`,
   `trusty-embedderd`; trusty-agents → `tagent`; trusty-code → `tcode`; trusty-analyze →
   `trusty-analyze`; trusty-review → `trusty-review`; trusty-kb → `trusty-kb`;
   trusty-channels → `slack-mcp`,`telegram-mcp`; trusty-gworkspace →
   `trusty-gworkspace-mcp`; trusty-sld-lint → `sld-lint`; trusty-publish-guard →
   `publish-guard`; trusty-console → `trusty-console` (standalone `[[bin]]`,
   `Cargo.toml:33-35`); **trusty-git-analytics → package name is literally `tga`, so
   `cargo install tga`**; ~~**trusty-mpm → `tm`/`trusty-mpm` but `publish = false`, NOT
   installable from crates.io**~~ **[FALSE — struck 2026-08-04, plan P8-T5; see the
   correction at the end of this item]**. `docs/reference/release-workflow.md:450-460` is
   **stale** — claims trusty-console is bundled with no standalone binary;
   `release.yml:356-368,1690` shows it released as its own artifact (de-bundled by
   #1318).

   > **CORRECTION 2026-08-04 (plan P8-T5) — the `trusty-mpm` half of this item is
   > FALSE.** Struck above, not deleted: this file is the record of what the
   > fact-check concluded, and the reversal is part of that record.
   > `crates/trusty-mpm/Cargo.toml` carries **no `publish` key**, so cargo defaults
   > to `publish = true`; `cargo search trusty-mpm` returns **1.3.4** as of
   > 2026-08-04. Phase 7 of the implementation plan installed it from crates.io in a
   > clean VM with `cargo install trusty-mpm --locked` and landed `tm` and
   > `trusty-mpm` — the outcome this line called impossible. DOC-1 D2 was reversed
   > on 2026-07-31; every other claim in this item is unaffected and stands.

7. **Repo is PUBLIC** — `{"nameWithOwner":"bobmatnyc/trusty-tools","visibility":"PUBLIC"}`.
   No GH token needed for read-only branch/local patterns.
