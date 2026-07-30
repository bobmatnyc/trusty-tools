# VM install-testing probe — findings

**Status:** in progress (exploratory probe, not a design doc)
**Date:** 2026-07-30
**Host:** Darwin 25.5 (macOS Tahoe 26.5), Apple Silicon, 18 logical cores, 64 GB RAM, ~536 GiB free
**Tooling:** `tart` v2.32.1 (Homebrew), base image `ghcr.io/cirruslabs/macos-tahoe-base:latest`
**Instrument:** `scripts/vmtest/bake-golden.sh` (probe artifact; NOT production tooling)

The purpose of this probe is to decide whether a Tart-based clean-machine harness is
viable for install-testing trusty-tools, and to replace unmeasured guesses with
measurements. **The measurements are the deliverable.** No harness is being built.

## Answer table

| # | Question | Answer | Confidence |
|---|----------|--------|------------|
| A | Base image inventory | **ANSWERED** — see A below | measured |
| B | `tart exec` viability / exit-code propagation | NOT YET MEASURED | — |
| C | Tahoe "Local Network" permission vs SSH | NOT YET MEASURED | — |
| D | Non-interactive access mechanics | NOT YET MEASURED | — |
| E | mise + Rust reality check | **ANSWERED — assumption was broken, see E** | measured |
| F | TCC / permission dialogs, headless | PARTIAL — `--no-graphics` boots and provisions | partial |
| G | Timings & disk | PARTIAL — golden→clone 0.30 s | partial |
| H | Compile-speed calibration | NOT YET MEASURED | — |
| I | Suspend/resume | NOT YET MEASURED | — |

---

## A. Base image inventory — ANSWERED

Captured inside a booted clone of `tahoe-base` before any provisioning.

```
## uname / version
ProductName:		macOS
ProductVersion:		26.5
BuildVersion:		25F71
arm64
## cpu/mem
logicalcpu=4 mem_gb=8
## disk
Filesystem        Size    Used   Avail Capacity iused ifree %iused  Mounted on
/dev/disk2s1s1    46Gi    12Gi    16Gi    43%    459k  169M    0%   /
## preinstalled tool presence
PRESENT mise -> /opt/homebrew/bin/mise
PRESENT brew -> /opt/homebrew/bin/brew
PRESENT git  -> /usr/bin/git
PRESENT gh   -> /opt/homebrew/bin/gh
ABSENT  tmux
PRESENT curl -> /usr/bin/curl
ABSENT  claude
ABSENT  rustc
ABSENT  cargo
ABSENT  rustup
ABSENT  uv
PRESENT node    -> /opt/homebrew/bin/node
PRESENT python3 -> /usr/bin/python3
PRESENT jq      -> /usr/bin/jq
PRESENT cmake   -> /opt/homebrew/bin/cmake
## pre-existing rust state
ls: /Users/admin/.rustup: No such file or directory
ls: /Users/admin/.cargo: No such file or directory
## mise version
2026.6.0 macos-arm64 (2026-06-03)
mise WARN  mise version 2026.7.18 available
```

Key points:

* **`mise` IS preinstalled** (Homebrew, `/opt/homebrew/bin/mise`). The research-track
  claim was correct. Do **not** `curl https://mise.run | sh` — that would create a
  second, conflicting mise in `~/.local/bin`.
* `mise self-update` **hard-fails** on a Homebrew-managed mise. Use `brew upgrade mise`.
* `gh` 2.93.0 and `node` are preinstalled from Homebrew — installing them via mise is
  redundant.
* `tmux` and `claude` are **absent**, which is exactly what a clean-machine harness needs
  in order to exercise installer auto-install-on-missing paths.
* No pre-existing `~/.cargo` or `~/.rustup` — the base image carries no Rust state.
* **Guest defaults are small: 4 vCPU / 8 GB RAM.** This is the single most important
  number for compile-time estimates (see H).

---

## E. mise + Rust reality check — ANSWERED, and an assumption WAS broken

`mise use -g rust@1.91` works and resolves to a genuinely pinned toolchain:

```
mise rust@1.91.1     [1/3] install
mise rust@1.91.1     [1/3] Downloading rustup-init
info: profile set to default
info: default host triple is aarch64-apple-darwin
info: syncing channel updates for 1.91.1-aarch64-apple-darwin
info: latest update on 2025-11-10 for version 1.91.1 (ed61e7d7e 2025-11-07)
info: downloading 6 components
mise rust@1.91.1     [1/3]   1.91.1-aarch64-apple-darwin installed - rustc 1.91.1 (ed61e7d7e 2025-11-07)
info: default toolchain set to 1.91.1-aarch64-apple-darwin
mise rust@1.91.1   ✓ installed
mise ~/.config/mise/config.toml tools: rust@1.91.1
...
## installed:
rust  1.91.1 (symlink)  ~/.config/mise/config.toml  1.91
uv    0.12.0            ~/.config/mise/config.toml  latest
```

**Confirmed: mise's rust backend delegates to rustup under the hood.** It downloads
`rustup-init`, installs a real rustup toolchain, and mise's own entry is listed as a
`(symlink)`. The real toolchain lives in `~/.cargo/bin` / `~/.rustup`, not in a mise-owned
install dir. `rust@1.91` resolved to `1.91.1`, which satisfies the workspace
`rust-version = "1.91"` floor.

### THE TRAP (confirmed): mise activation does not reach non-interactive shells

This was flagged as a likely trap and it is real. `tart exec <vm> /bin/sh -c 'cargo …'`
returns **127** — even with a login shell — because mise's shell activation is written
into interactive-shell rc files that a non-interactive `sh` never reads. Any automation
driving the guest the way `tart exec` or `ssh host cmd` does would silently fail to find
the toolchain.

**Working fix used in the bake script:** write `~/.zshenv` and drive the guest with
`/bin/zsh`, never `/bin/sh`. zsh reads `~/.zshenv` on *every* invocation — interactive or
not, login or not — which is exactly the shell form `ssh host cmd` and `tart exec` use.

```sh
cat > ~/.zshenv <<'INNER'
export PATH="$HOME/.cargo/bin:$HOME/.local/share/mise/shims:$PATH"
INNER
```

Verified in a **fresh non-interactive shell**:

```
PATH=/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
rustc: rustc 1.91.1 (ed61e7d7e 2025-11-07)
cargo: cargo 1.91.1 (ea2d97820 2025-10-10)
uv:    uv 0.12.0 (b88d7c5c4 2026-07-28 aarch64-apple-darwin)
gh:    gh version 2.93.0 (2026-05-27)
```

Note the PATH puts `~/.cargo/bin` **ahead of** the mise shims dir: since mise delegates to
rustup anyway, going straight to the rustup binaries avoids a shim indirection on every
`cargo` invocation.

Alternatives not needed here but valid: `mise exec -- cargo …` or
`mise activate --shims`.

---

## F. TCC / headless — PARTIAL

`tart run --no-graphics` boots the guest successfully and the guest agent becomes
responsive; a full mise + rustup provisioning run completed headless with **no observed
TCC dialog**. A graphical-vs-headless comparison and an explicit check for dialogs is
NOT YET MEASURED.

---

## G. Timings & disk — PARTIAL

| Measurement | Value |
|---|---|
| `tart clone` of **golden** (APFS CoW) | **0.30 s** |
| `tart clone` of base | NOT YET MEASURED |
| boot-to-ready (agent responsive) | NOT YET MEASURED |
| full mise + rust provisioning | NOT YET MEASURED |
| `tart stop` | NOT YET MEASURED |
| `tart delete` | NOT YET MEASURED |
| golden on-disk size after bake | NOT YET MEASURED |
| true CoW divergence cost of a clone | NOT YET MEASURED |

### Suspected broken assumption: `--disk-size` auto-expansion

`scripts/vmtest/bake-golden.sh` claims in its header that
`tart set --disk-size N` auto-expands the guest APFS container on next boot with no
in-guest `diskutil apfs resizeContainer`. The pristine inventory above was taken *after*
`tart set --disk-size 80` and a boot, and the guest reports a **46 GiB** filesystem — the
original ~50 GB image, not 80 GB. **This claim looks wrong and is flagged for
verification.** `tart list` reports Disk=80 for the golden, so the host-side container
grew but the guest filesystem apparently did not.

---

## B, C, D, H, I — NOT YET MEASURED

---

## What this changes about the design

*(to be completed as measurements land)*

1. **Provisioning must never be driven through `/bin/sh`.** Any harness that shells into
   the guest has to use `/bin/zsh` plus a `~/.zshenv` PATH export (or `mise exec --`).
   A `sh`-based runner would fail with exit 127 and no obvious cause.
2. **Do not install mise.** It ships with the base image; installing it again creates a
   conflicting second copy.
3. **Guest default sizing is 4 vCPU / 8 GB.** All prior compile-time estimates that
   assumed host-like parallelism are invalid on the default clone.

## Host state left behind

*(to be finalised at end of probe)*
