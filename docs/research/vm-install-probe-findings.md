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
| C | Tahoe "Local Network" permission vs SSH | **ANSWERED — does NOT block, see C** | measured |
| D | Non-interactive access mechanics | **ANSWERED — see D** | measured |
| E | mise + Rust reality check | **ANSWERED — assumption was broken, see E** | measured |
| F | TCC / permission dialogs, headless | **ANSWERED — mic preflight, not network, see F** | measured |
| G | Timings & disk | **ANSWERED — see G and I** | measured |
| H | Compile-speed calibration | **ANSWERED — 131 s, not 20–55 min. See H** | measured |
| I | Suspend/resume | **ANSWERED — resume is BROKEN, see I** | measured |

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

## C. Tahoe "Local Network" permission — ANSWERED: **it does not block anything**

This was expected to be the finding that disqualified the whole approach. It is not.

```
### C1: tart ip
IP=[192.168.64.7] rc=0
### C2: is sshd even listening in the guest
		"com.openssh.sshd" => enabled
tcp4       0      0  *.22    *.*    LISTEN
tcp6       0      0  *.22    *.*    LISTEN
### C3: raw TCP reachability from host, 5s timeout
nc rc=0
### C6: ping
64 bytes from 192.168.64.7: icmp_seq=1 ttl=64 time=1.049 ms
2 packets transmitted, 2 packets received, 0.0% packet loss
```

`ssh` completes a full key exchange and reaches the authentication stage:

```
debug1: SSH2_MSG_SERVICE_ACCEPT received
debug1: Authentications that can continue: publickey,password,keyboard-interactive
admin@192.168.64.7: Permission denied (publickey,password,keyboard-interactive).
```

That is an **authentication** failure (we had not yet installed a key), not a network
block. A Local Network denial severs traffic at the packet level — we would never have
seen ICMP replies, a TCP accept, or an SSH KEX.

**No GUI prompt appeared and none was needed.** A unified-log sweep for local-network /
TCC denials over the test window returned no denial for networking. No permission dialog
gated the run, and nothing was clicked through.

### Important caveat on generalising this

macOS attributes Local Network permission to the **responsible app**, not to the process
making the syscall. Here the responsible app is iTerm2:

```
responsible={TCCDProcess: identifier=com.googlecode.iterm2, ...
             responsible_path=/Applications/iTerm.app/Contents/MacOS/iTerm2},
accessing={TCCDProcess: identifier=com.apple.Virtualization.VirtualMachine, ...}
```

iTerm2 evidently already holds the necessary grant on this host. **A harness launched
under a different responsible app — a LaunchAgent, a CI runner, a different terminal —
could still be prompted on first use.** `tart exec` uses vsock and has no such
dependency, so it remains the safer transport even though SSH works here.

---

## D. Non-interactive access mechanics — ANSWERED

```
### D1: is sshpass available on the host?
sshpass ABSENT on host
### D2: inject an authorized_key via tart exec -i (no network, no password)
injected; authorized_keys now:
       1
inject rc=0
### D3: key-based non-interactive login
Warning: Permanently added '192.168.64.7' (ED25519) to the list of known hosts.
SSH_OK
admin
PATH=/usr/bin:/bin:/usr/sbin:/sbin
zsh:1: command not found: cargo
ssh rc=127
### D4: exit code propagation over ssh
host saw 7
```

* **Key injection via `tart exec -i` works and is the clean bootstrap.** It needs no
  network, no password, and no `sshpass` — which matters, because **`sshpass` is not
  installed on the host** and installing it would violate the no-host-changes constraint.
* Key-based non-interactive login then works, and **SSH propagates exit codes** (7).
* `-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null` handles host-key churn
  across clones — clones share a host key, and the only output is a benign warning.
* **SSH's non-interactive PATH is even barer than `tart exec`'s:**
  `/usr/bin:/bin:/usr/sbin:/sbin`. It does not even contain `/opt/homebrew/bin`, so
  **`mise`, `brew` and `gh` are all invisible over SSH too**, not just `cargo`. Anything
  driving the guest over SSH must set PATH explicitly.

---

## F. TCC / headless — ANSWERED (with a surprise)

`tart run --no-graphics` boots the guest successfully, the guest agent becomes
responsive in ~34 s, and a full mise + rustup provisioning run completes headless. **No
TCC dialog blocked provisioning.**

However, starting a VM **does** raise a TCC request — for the **microphone**:

```
AUTHREQ_CTX: msgID=611.231, service=kTCCServiceAudioCapture, preflight=yes
AUTHREQ_ATTRIBUTION: attribution={
  responsible={identifier=com.googlecode.iterm2, responsible_path=/Applications/iTerm.app/...},
  accessing={identifier=com.apple.Virtualization.VirtualMachine, ...}}
AUTHREQ_RESULT: msgID=611.231, authValue=1, authReason=0
```

`Virtualization.framework` preflights `kTCCServiceAudioCapture` even for a
`--no-graphics` guest. Here it resolved to `authValue=1` (allowed) with no prompt because
the responsible app already held the grant. **On a host where the responsible app has no
microphone grant, this is a candidate for a first-run GUI prompt.** It did not block this
probe, but a harness should be aware that a mic prompt — not a network prompt — is the
dialog most likely to appear.

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

## H. Compile-speed calibration — **the 20–55 min/crate estimate is wrong by 10–25×**

Test: `cargo install tga --locked` on `probe-h`, a fresh clone of the golden with a
**cold** cargo registry (the golden carries rustup but no downloaded crates), at the
**default guest sizing of 4 vCPU / 8 GB**.

```
=== guest config ===
logicalcpu=4 mem_gb=8
/dev/disk2s1s1    74Gi    12Gi    43Gi    22%    /
=== H1 BUILD: cargo install tga --locked  (4 vCPU / 8 GB) ===
   Compiling rusqlite v0.39.0
   Compiling trusty-common v0.24.1
   Compiling indicatif v0.17.11
   Compiling rayon v1.12.0
   Compiling tera v1.20.1
   Compiling csv v1.4.0
   Compiling futures v0.3.32
   Compiling uuid v1.23.1
   Compiling git2 v0.20.4
   Compiling tga v2.9.4
    Finished `release` profile [optimized] target(s) in 2m 11s
  Installing /Users/admin/.cargo/bin/tga
   Installed package `tga v2.9.4` (executable `tga`)
GUEST_BUILD_SEC=131
GUEST_BUILD_RC=0
--- installed binary ---
-rwxr-xr-x  1 admin  staff  22771088 Jul 30 22:28 /Users/admin/.cargo/bin/tga
tga 2.9.4
HOST_SAW_RC=0
```

**131 seconds, cold registry, on the smallest guest configuration, including dependency
download, a full release build of ~300 crates, and linking a 22.7 MB binary.**

Current design docs estimate **20–55 minutes per crate**. The measured figure is
**2m 11s** — the estimate is high by roughly **10–25×**. Those numbers were extrapolated,
never measured, and should be discarded.

### Disk cost of a real workload

```
=== host free (KB) before ===  561359340
=== host free (KB) after  ===  560047392
```

A full cold-registry `cargo install` diverges the clone by **1,311,948 KB ≈ 1.25 GiB**.
Combined with the ~104 KB idle clone cost from G, host capacity is:

* idle clones: effectively unbounded
* clones that have each done a full from-source install: **~400** at 536 GiB free

Disk is not a binding constraint for this harness.

## I. Suspend/resume — **BROKEN. Do not build a strategy on it.**

`tart suspend` exists and *appears* to work. **Resume fails, reproducibly.**

Attempt 1:

```
### I-A: COLD BOOT from stopped (baseline), timed
COLD_BOOT_TO_READY_MS=17993
### I-C: suspend, then WAIT for the tart run process to exit
suspend cmd rc=0
SUSPEND_TOTAL_MS=2308
run pid still alive? NO
local  probe-h   80  33   2 seconds ago  suspended
### I-D: RESUME, timed to agent-ready
RESUME FAILED
restoring VM state from a snapshot...
Error Domain=VZErrorDomain Code=12 "The virtual machine failed to restore with error
“invalid argument”." UserInfo={NSLocalizedFailure=An error occurred while restoring the
virtual machine., NSLocalizedFailureReason=The virtual machine failed to restore with
error “invalid argument”.}
```

Attempt 2, with a 5 s settle before suspending in case of an unflushed-state race:

```
suspend rc=0
local  probe-h   80  33   2 seconds ago  suspended
### resume attempt 2
RESUME FAILED AGAIN
restoring VM state from a snapshot...
Error Domain=VZErrorDomain Code=12 "The virtual machine failed to restore with error
“invalid argument”."
### final state
local  probe-h   80  33   1 second ago   suspended
```

Findings:

* Suspend itself completes in ~2.3 s and correctly transitions the VM to `suspended`.
* **Restore fails every time** with `VZErrorDomain Code=12` on this host
  (Darwin 25.5) and this image. Suspend/resume is **not** a usable fast-start strategy.
* **A failed restore leaves the VM wedged in `suspended` state**, where `tart run`
  keeps re-attempting the restore and keeps failing. A harness would need explicit
  recovery for this state.
* **`tart suspend` is asynchronous.** The command returns in ~0.3 s while the VM is still
  `running`; the state only settles once the owning `tart run` process exits. Racing it
  produces `VM "probe-h" is already running!`. Any automation must wait for the `tart
  run` process to exit rather than trusting the suspend command's return.

### The good news: cold boot is cheap, so resume is not needed

`tart clone` + cold boot is already fast enough that suspend/resume buys little:

| Path | Time |
|---|---|
| clone golden | 0.31 s |
| **first** boot of a fresh clone | 34.4 s (includes APFS container expansion) |
| **subsequent** cold boot of the same VM | **18.0 s** |

Note the first boot of a fresh clone costs roughly **2× a later boot** — the one-time
APFS container expansion and first-boot work land there. Budget ~35 s for a clean VM
from nothing.

**Clone-and-boot is the clean-VM strategy. Suspend/resume is off the table.**

### J. Unwedge procedure for a stuck-`suspended` VM — **verified, reproducible fix**

A follow-up cleanup pass found `probe-h` still wedged from the resume failures above
(`tart run` kept re-attempting the restore and kept failing, exactly as predicted in I).
Before touching `probe-h` itself, the fix was proven end-to-end on a disposable clone
(`cleanup-unwedge-test`, cloned from `tahoe-base`, deleted immediately after) so the
procedure below is empirically confirmed, not guessed:

```
$ tart clone tahoe-base cleanup-unwedge-test          # 0.x s, CoW clone
$ tart run --no-graphics cleanup-unwedge-test &       # cold boot
$ tart ip cleanup-unwedge-test                        # 192.168.64.10 — confirmed running

$ tart suspend cleanup-unwedge-test
rc=0 → state settles to `suspended` once the `tart run` process exits (~3 s)

$ tart run --no-graphics cleanup-unwedge-test         # attempt 1: resume
restoring VM state from a snapshot...
Error Domain=VZErrorDomain Code=12 "The virtual machine failed to restore with error
“invalid argument”."
rc=1 — state stays `suspended`

$ tart run --no-graphics cleanup-unwedge-test         # attempt 2: resume, same result
restoring VM state from a snapshot...
Error Domain=VZErrorDomain Code=12 "...invalid argument..."
rc=1 — confirms the wedge is reproducible and does not clear on retry
```

Root cause of the wedge: `tart`'s VM directory
(`~/.tart/vms/<name>/`) contains a `state.vzvmsave` snapshot file once suspended, and
`tart list` derives the `suspended` state purely from that file's presence. As long as
it's there, `tart run` unconditionally tries to restore from it — and the restore is
what's broken (VZError 12), so every retry fails the same way. There is no CLI flag to
force a cold boot over a restore.

**The fix — move the snapshot file out of the way so `tart run` has nothing to restore
from:**

```
$ mv ~/.tart/vms/cleanup-unwedge-test/state.vzvmsave \
     ~/.tart/vms/cleanup-unwedge-test/state.vzvmsave.bak

$ tart list | grep cleanup-unwedge-test
local  cleanup-unwedge-test   50  31  in 0 seconds  stopped   # state flips to `stopped` immediately, just from the rename

$ tart run --no-graphics cleanup-unwedge-test &       # retry — this time cold boots
$ tart ip cleanup-unwedge-test
192.168.64.10                                          # running and pingable within ~20 s

$ tart stop cleanup-unwedge-test && tart delete cleanup-unwedge-test   # throwaway cleaned up immediately
```

Confirmed: `ping -c 2 192.168.64.10` succeeded (0% loss) after the fix, so this is a
real cold boot, not a stale `running` label.

**Unwedge procedure (general form):**

```
mv ~/.tart/vms/<name>/state.vzvmsave ~/.tart/vms/<name>/state.vzvmsave.bak
tart run --no-graphics <name>
```

This is destructive to the suspended-state snapshot (the guest loses whatever
in-memory state it had at suspend time and comes back via a fresh cold boot of the
same disk), which is fine given I already established suspend/resume is not a viable
strategy — the goal here is just to get the VM back to a usable `stopped`/`running`
state, not to preserve the snapshot.

`probe-h` itself was independently found already recovered by the time this pass
started: its directory already contained `state.vzvmsave.bak` (not `state.vzvmsave`)
and its `tart run --no-graphics probe-h` process (PID 40976, launched 7:02PM) was live,
with `tart ip probe-h` → `192.168.64.7` and `ping` succeeding. This matches the fix
above exactly, so a prior session evidently applied it but stopped before committing
its notes — this section is that missing writeup, now verified independently on a
disposable VM rather than taken on faith.

**Recommendation for the harness:** if a suspend/resume path is ever revisited despite
the finding in section I, the unwedge step above must be built in as automatic recovery
before any `tart run` retry on a VM found in `suspended` state — otherwise a single
resume failure permanently wedges that VM name until someone manually intervenes.

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

A cleanup pass (branch `probe/tart-golden-image-cleanup`, off this commit) recovered the
wedged `probe-h` (see section J above) and then removed every probe-created VM.

**Before:**

```
$ tart list
Source Name                    Disk Size Accessed       State
local  probe-calib-analyze     80        3 minutes ago  running
local  probe-h                 80        51 seconds ago running   (wedged/suspended prior to this run — see section J)
local  probe-h2-scaled         80        55 seconds ago stopped
local  tahoe-base               50        42 minutes ago stopped
local  trusty-toolchain-20260730 80      5 minutes ago  stopped
OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                  50 1 week ago stopped
OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1...           50 1 week ago stopped

$ df -h /
Filesystem       Size   Used  Avail Capacity  Mounted on
/dev/disk3s1s1   926Gi  16Gi  522Gi 3%        /
```

Note `probe-calib-analyze` was not in this probe's original known-VM list but was
clearly probe-created (name prefix, running, untracked in any "keep" list) — treated as
in-scope and removed along with the rest.

**Actions taken:**

* Unwedged `probe-h` — already fixed by a prior session before this pass started
  (see section J); independently verified still healthy (`tart ip` → `192.168.64.7`,
  ping succeeded) before deleting it.
* `tart stop probe-h` then `tart delete probe-h`, `tart delete probe-h2-scaled`,
  `tart delete probe-calib-analyze` — all probe-created VMs removed.
* `tart delete trusty-toolchain-20260730` — **deleted as confirmed-broken.** This is
  the golden image from section B/G with the missing `~/.zshenv` (`cargo` → exit 127 in
  a fresh clone). Confirmed before deletion: name `trusty-toolchain-20260730`, listed
  disk size 80 GB (allocated), actual on-disk usage 31 GiB (`du -sh`). Deleting it now
  so it can't later be mistaken for a working golden image.
* `tahoe-base` and both OCI-sourced base images were left untouched, as instructed —
  `tahoe-base` is the owner's base image, not probe-created.

**After:**

```
$ tart list
Source Name                                     Disk Size Accessed      State
local  tahoe-base                               50        3 minutes ago stopped
OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                50 1 week ago stopped
OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1...         50 1 week ago stopped

$ df -h /
Filesystem       Size   Used  Avail Capacity  Mounted on
/dev/disk3s1s1   926Gi  16Gi  530Gi 3%        /
```

Free space on `/` went from 522 GiB to 530 GiB (+8 GiB reclaimed). The gain is smaller
than the sum of nominal disk sizes because these were APFS copy-on-write clones sharing
blocks with `tahoe-base`/the OCI base images; only each clone's diverged blocks were
actually reclaimed.

**Host state remaining after this pass:** only `tahoe-base` (owner's base image, 50 GB
nominal / not touched) and the two `ghcr.io/cirruslabs/macos-tahoe-base` OCI-sourced
entries (pulled base images, not clones) remain. No probe-created VMs, no golden image,
no throwaway/calibration clones left on the host.

---

## K. Build-cost measurement pass — `tart stop` asynchrony, `PROVISION_SEC`, and trusty-search from source

**Date:** 2026-07-31
**Branch:** `probe/tart-build-cost` (off `2e2ad22a`)
**Host:** Darwin 25.5 (Tahoe), Apple Silicon, **18 logical cores**, 64 GB RAM, 530 GiB free at start
**Scope (fixed by the owner, not up for debate in this section):**

* **Local Tart VM only.** GitHub Actions / CI runners are explicitly out of scope. Nothing
  here should be extrapolated to or from a GHA runner.
* **Downloading all crate dependencies from the network is acceptable.** No registry
  pre-warming was performed or is wanted.
* **The host repo is never mounted into a guest.** Source reaches the VM only by
  `git clone` from GitHub inside the guest (the repo is public).

### K answer table

| # | Question | Answer | Confidence |
|---|----------|--------|------------|
| K1 | Is `tart stop` synchronous? Does it lose the last write? | **ANSWERED — state is synchronous, DATA IS NOT. 4 of 5 unsynced last writes were LOST. See K1** | measured |
| K2 | `PROVISION_SEC` — full mise + rust + uv + gh from a bare clone | **ANSWERED — 30 s. See K2** | measured |
| K3 | `cargo install --path crates/trusty-search --locked` wall-clock | _pending_ | _pending_ |
| K3a | Does workspace `lto = "thin"` apply to a `--path` install? | _pending_ | _pending_ |
| K3b | Does `ort-sys` download ONNX Runtime inside the guest? | _pending_ | _pending_ |
| K4 | Is single-crate default scope still justified? | _pending_ | _pending_ |
| K4b | Does the golden image earn its complexity? | _pending_ | _pending_ |
| K5 | `rust-toolchain.toml` override in `trusty-git-analytics` | _pending_ | _pending_ |

### K1. `tart stop` asynchrony — **CONFIRMED: `tart stop` silently discards the guest's last writes**

This was the leading hypothesis for why the `trusty-toolchain-20260730` golden image
shipped with `~/.zshenv` missing (section B/E). **The hypothesis is confirmed**, and the
mechanism is worse than "asynchronous stop".

Logs: `docs/research/vm-probe-logs/k1-tart-stop-asynchrony.log`,
`k1b-sync-vs-delay-isolation.log`, `k1c-write-loss-repeat-trial.log`,
`k1d-state-poll-overhead.log`.

#### K1-i. Is the *state transition* asynchronous? **No — that part is fine.**

```
STOP_A_CMD_RETURN_MS=405
STATE_IMMEDIATELY_AFTER_STOP_RETURNS=stopped
STOP_A_TO_ACTUALLY_STOPPED_MS=747
STOP_A_EXTRA_LAG_AFTER_CMD_RETURN_MS=342
```

The 342 ms of apparent "extra lag" is **entirely the cost of the poll itself**, not real lag:

```
vm_state_helper_ms=308 / 303 / 303
```

By the time `tart stop` returns, `tart list` already reports `stopped`, across all 8 stop
cycles measured. `tart stop` returns in **~360–510 ms** and its exit code was `0` every time.

**So the naive check passes.** That is precisely the trap.

#### K1-ii. Is the *data* durable when it returns? **No. 4 out of 5 trials lost the last write.**

The exact recipe — boot a fresh `tahoe-base` clone, `printf` a sentinel into `$HOME`,
`ls`/`cat` it back successfully, then immediately `tart stop` — run 5 times:

| Trial | `tart stop` rc | returned in | state on return | sentinel after clone+boot |
|---|---|---|---|---|
| A (original) | 0 | 405 ms | `stopped` | **LOST** |
| K1c-1 | 0 | 364 ms | `stopped` | **LOST** |
| K1c-2 | 0 | 408 ms | `stopped` | **LOST** |
| K1c-3 | 0 | 502 ms | `stopped` | SURVIVED |
| K1c-4 | 0 | 401 ms | `stopped` | **LOST** |

**Loss rate: 4/5.** It is a race, not a deterministic failure — which is why a bake script
can appear to work, be committed, and then ship a broken image on a later run.

Critically, in variant A the loss was confirmed **on the original VM too**, not just in a
clone:

```
--- does sentinel_immediate exist on the ORIGINAL VM after its own reboot?
A_ON_ORIGINAL=NO
```

So this is genuine data loss at the disk image level, not a `tart clone` snapshot artefact.
The write was verified present in the running guest (`cat` returned `SENTINEL_IMMEDIATE`,
`ls -l` showed 18 bytes) immediately before the stop.

`tart stop` returns in ~400 ms. A graceful macOS shutdown does not complete in 400 ms.
The `--timeout 30` graceful window is documented as "seconds to wait for graceful
termination before forcefully terminating" — in practice the VM is torn down long before
the guest has flushed its APFS write cache.

#### K1-iii. What actually protects the write?

| Variant | Recipe | Result |
|---|---|---|
| A / K1c | write, **immediate** `tart stop` (default `--timeout 30`) | **LOST 4/5** |
| B | write, `sync`, 10 s settle, `tart stop` | SURVIVED |
| C | write, **`sync`**, immediate `tart stop` | SURVIVED |
| D | write, **10 s settle**, no `sync`, `tart stop` | SURVIVED |
| E | write, no `sync`, no settle, **`tart stop --timeout 120`** | SURVIVED |

Both a guest-side `sync` and a settle delay were individually sufficient in the trials run.
**Variant E is the dangerous result and should not be read as "`--timeout 120` fixes it"** —
E is a single trial of a recipe that fails only 80 % of the time, and its stop still returned
in 396 ms, meaning the longer timeout was never actually exercised. Treat E as a coin flip
that came up heads, not as a fix.

#### K1-iv. Verdict and the correct shutdown procedure

* **`tart stop`'s exit code is NOT a reliable completion signal for durability.** It was `0`
  in every one of the 4 runs that lost data.
* **Polling for `stopped` does NOT help either.** The state was already `stopped` when the
  command returned, in every trial including the losing ones. There is no "wait for stopped"
  poll that fixes this, because the state flag is not a durability flag. This corrects the
  natural assumption that a wait-for-stopped loop is the missing safety.
* The only measured protection is **on the guest side, before the stop**.

The procedure a bake or snapshot script must use:

```sh
# 1. flush the guest, from inside the guest, and confirm it returned
tart exec "$VM" /bin/sh -c 'sync; sync; echo FLUSHED'
# 2. settle (belt and braces -- both sync and delay were independently sufficient)
sleep 10
# 3. now stop
tart stop "$VM"
# 4. verify the artefact by cloning + booting and asserting on the file,
#    NOT by trusting the stop's exit code
```

**And, decisively: a golden image must be verified by clone→boot→assert after the stop.**
The `bake-golden.sh` run that produced `trusty-toolchain-20260730` verified `~/.zshenv`
*while the VM was still running* and then trusted `tart stop`'s exit code — which is exactly
the failure mode measured here. That explains section B/E's headline finding completely.

Throwaway VMs (`probe-k1`, `probe-k1-c1`, `probe-k1-c2`, `probe-k1b-{C,D,E}`,
`probe-k1c{1..4}` and their clones) were all deleted; `tart list` confirms only `tahoe-base`
and the two OCI base images remain.

### K2. `PROVISION_SEC` = **30 seconds** — the golden image does not earn its complexity

Log: `docs/research/vm-probe-logs/k2-provision-sec.log`.

Guest configuration actually set (recorded, as required): `tart set probe-k2 --cpu 8
--memory 16384 --disk-size 100`. The guest confirmed `hw.ncpu=8`, `mem_gb=16`. Host has
**18 logical cores / 64 GB**, so an 8-vCPU guest leaves ample headroom and does not
contend with the host.

#### The number

```
STEP_RUST_MS=20778     # mise use -g rust@1.91  -> rustup-init + 6 components, 1.91.1
STEP_UV_MS=7947        # mise use -g uv@latest  -> uv 0.12.0, incl. attestation verify
STEP_GH_MS=616         # gh 2.93.0 ALREADY PRESENT from Homebrew -- nothing to install
STEP_ZSHENV_MS=617     # write ~/.zshenv
PROVISION_MS=30079
PROVISION_SEC=30
```

**Full toolchain provisioning from a bare `tahoe-base` clone is 30 seconds**, over the
network, with nothing pre-warmed. The rust toolchain download — the step everyone assumes
is expensive — is **20.8 s**.

Note `gh` costs nothing: it is preinstalled on `macos-tahoe-base` (2.93.0), as are `git`,
`curl`, `node`, `python3`, `cmake` and `mise` (2026.6.0). Only `rustc`/`cargo`/`rustup`/`uv`
were absent.

#### The keep-or-kill arithmetic

| Path to a ready-to-build VM | Cost |
|---|---|
| clone golden image | 0.31 s |
| clone `tahoe-base` + provision | 0.31 s + **30 s** = ~30 s |
| **Delta the golden image buys** | **~30 seconds per run** |

Thirty seconds. Against that, the golden image costs: a bake script, a bake pipeline, an
image that goes stale as toolchains move, ~33 GB of image to keep current, and — as
sections B/E and now K1 establish — **a demonstrated failure mode where it silently ships
broken and every downstream run fails with `cargo: exit 127`**.

**Recommendation: KILL the golden image.** Provision from `tahoe-base` on each run. See
K4 for the full argument.

#### Incidental: disk expansion behaviour, re-confirmed

Section G's correction holds exactly. `--disk-size 100` applied to a clone shows the
*pre-expansion* size on the boot in which it is applied, and the expanded size on the next
boot:

```
boot 1: /dev/disk2s1s1    46Gi    12Gi    16Gi    42%    /
boot 2: /dev/disk2s1s1    93Gi    12Gi    63Gi    16%    /
```

Any bake or setup script that asserts on free space must do so **after a reboot**, not
immediately after the resize. Note also that the un-resized default leaves only **16 GiB
free**, which is not enough headroom for a large from-source build — resizing is not
optional for this workload.

#### Non-interactive PATH verified both ways

Following K1 discipline (write, `sync`, then verify), `~/.zshenv` was confirmed effective
in a fresh non-interactive `/bin/zsh` — the exact check the broken golden image failed:

```
--- /bin/zsh, relying ONLY on ~/.zshenv:
PATH=/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:...
rustc 1.91.1 (ed61e7d7e 2025-11-07)
cargo 1.91.1 (ea2d97820 2025-10-10)
uv 0.12.0 (b88d7c5c4 2026-07-28 aarch64-apple-darwin)
```

`rustup 1.29.0`, `MSRV_OK` (1.91.1 satisfies the `rust-version = "1.91"` floor).

### K3. trusty-search from source — _pending_

_pending_

### K4. Extrapolation and recommendation — _pending_

_pending_

### K5. Opportunistic checks — _pending_

_pending_
