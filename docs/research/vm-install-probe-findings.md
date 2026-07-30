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
| B | `tart exec` viability / exit-code propagation | **ANSWERED — fully viable, see B** | measured |
| C | Tahoe "Local Network" permission vs SSH | NOT YET MEASURED | — |
| D | Non-interactive access mechanics | NOT YET MEASURED | — |
| E | mise + Rust reality check | **ANSWERED — assumption was broken, see E** | measured |
| F | TCC / permission dialogs, headless | PARTIAL — `--no-graphics` boots and provisions | partial |
| G | Timings & disk | **MOSTLY ANSWERED — see G** | measured |
| H | Compile-speed calibration | NOT YET MEASURED | — |
| I | Suspend/resume | NOT YET MEASURED | — |

> **Headline finding (see B/E):** the baked golden image
> `trusty-toolchain-20260730` does **not** actually have a working non-interactive
> PATH. `~/.zshenv` — the fix the bake script documents and the bake run verified —
> is **absent from the produced image**. A clone of the golden today answers
> `cargo --version` with exit **127** under both `/bin/sh` and `/bin/zsh`.

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

## B. `tart exec` viability — ANSWERED, fully viable

All tests run against `probe-h`, a clone of the golden. Raw output:

```
### B1: exit code propagation, /bin/sh, exit 7
host saw 7
### B2: exit code 0
host saw 0
### B3: exit code 1 from a failing real command
host saw 1
### B4: stdout captured?
captured=[HELLO_STDOUT]
### B5: stderr separation (stdout only)
stdout_only=[TO_OUT]
### B6: -i stdin heredoc piping
heredoc ran as admin in /Users/admin
arg-free script works
host saw 3 (expect 3 if exit codes propagate through -i)
### B7: cargo visible via /bin/sh (the E trap) ?
sh saw 127
### B8: cargo visible via /bin/zsh ?
zsh saw 127
### B9: long output truncation - expect 200000 lines
lines_received=  200000
last_line=200000
### B10: streaming vs buffering - timestamps of arrival
123.23 tick1
125.28 tick2
127.3 tick3
```

Conclusions:

* **Exit codes propagate exactly**, including through the `-i` stdin form (`exit 3` →
  host saw 3). This is the single most important property for a test harness and it
  holds.
* **stdin piping works.** A heredoc fed to `tart exec -i <vm> /bin/zsh -s` runs as
  `admin` in `/Users/admin`. This is a complete substitute for file transfer for
  provisioning scripts.
* **stdout and stderr are separate streams** on the host side (B5 discarded stderr and
  kept stdout cleanly).
* **Output streams, it does not buffer.** B10's three ticks arrived at 123.23 / 125.28 /
  127.30 — 2 s apart, matching the guest's `sleep 2`. Long-running commands give live
  output.
* **No truncation.** 200 000 lines sent, 200 000 received, last line intact.

`tart exec` is a viable harness transport. It needs no network, no SSH, no credentials.

### B7/B8 — the golden image is broken

`cargo` is invisible to **both** `/bin/sh` **and** `/bin/zsh` on a fresh clone:

```
### does ~/.zshenv exist?
---
rc=1
### PATH seen by zsh -c
PATH=/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
### PATH seen by zsh -s (stdin form)
PATH=/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
cargo:
```

The toolchain itself is present and functional — only the PATH wiring is missing:

```
### explicit absolute path invocation
cargo 1.91.1 (ea2d97820 2025-10-10)
rc=0
### rustup default toolchain
1.91.1-aarch64-apple-darwin (default)
rustc 1.91.1 (ed61e7d7e 2025-11-07)
```

Everything else provisioning wrote **did** persist — `~/.cargo` (22:15), `~/.rustup`
(22:15), `~/.config/mise/config.toml` (22:16, `rust = "1.91"`), and the mise shims dir.
`~/.zshenv` is the *only* provisioning artifact missing, and it was the *last* thing the
provisioning script wrote. The bake run's own `verify.txt` proves it existed and worked
at bake time (it printed the prepended PATH). Something between verify and the produced
image dropped it. Root cause is not yet established; a persistence experiment is pending.

**Design consequence:** a harness must not depend on guest shell rc files at all.
Since `tart exec` has **no `--env` flag**, the reliable pattern is to prefix PATH
explicitly inside the command itself, or invoke absolute paths:

```sh
tart exec "$VM" /bin/zsh -c 'export PATH="$HOME/.cargo/bin:$PATH"; cargo --version'
```

---

## F. TCC / headless — PARTIAL

`tart run --no-graphics` boots the guest successfully and the guest agent becomes
responsive; a full mise + rustup provisioning run completed headless with **no observed
TCC dialog**. A graphical-vs-headless comparison and an explicit check for dialogs is
NOT YET MEASURED.

---

## G. Timings & disk — MOSTLY ANSWERED

| Measurement | Value |
|---|---|
| `tart clone` of **golden** (80 GB image) | **0.310 s** |
| `tart clone` of **base** (50 GB image) | **0.306 s** |
| `tart delete` | **0.29–0.30 s** |
| boot-to-ready (headless clone, agent responsive) | **34.4 s** |
| golden `du -sh` | 31 G (misleading — see below) |
| golden `disk.img` apparent size | 80,000,000,000 bytes |
| **true CoW divergence cost of a clone** | **~104 KB** |
| full mise + rust provisioning | NOT YET MEASURED (bake stdout was lost) |
| `tart stop` | NOT YET MEASURED |

Raw:

```
CLONE_BASE_MS=306
CLONE_GOLDEN_MS=310
DELETE_MS=299
DELETE2_MS=293
BOOT_TO_READY_MS=34414
```

### Clone cost is effectively zero and independent of image size

Cloning the 50 GB base and the 80 GB golden take the same ~0.31 s. This is an APFS
`clonefile`, not a copy. Real disk consumed by making **two** clones:

```
=== df Data avail KB before ===  561725380
=== df Data avail KB after 2 clones ===  561725172
```

208 KB total for two clones, i.e. **~104 KB per clone**. Host capacity for clean VMs is
therefore *not* bounded by clone count — it is bounded only by how much each clone
*writes* after divergence. With 536 GiB free the host can hold effectively unlimited
idle clones.

**`du -sh` is not a usable measure here.** It reports 31 G for both the golden and its
clone because it counts CoW-shared blocks against each. Use `df` deltas instead.

### Corrected: `--disk-size` auto-expansion DOES work (just not on the resize boot)

An earlier reading of this probe's own data suggested `tart set --disk-size 80` failed to
expand the guest filesystem, because the bake run's pristine inventory reported a 46 GiB
volume. That reading was **wrong**. On a later boot the guest reports:

```
Filesystem        Size    Used   Avail Capacity  Mounted on
/dev/disk2s1s1    74Gi    12Gi    43Gi    22%    /
...
/dev/disk0 (internal, physical):
   0:      GUID_partition_scheme       *80.0 GB    disk0
   2:                 Apple_APFS Container disk2   79.5 GB   disk0s2
```

So the header claim in `bake-golden.sh` holds — no in-guest `diskutil apfs
resizeContainer` is needed — but the expansion is **not visible during the boot in which
the resize is first applied**. A bake script that asserts on free space immediately after
resizing will read the pre-expansion number.

---

## C, D, H, I — NOT YET MEASURED

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
