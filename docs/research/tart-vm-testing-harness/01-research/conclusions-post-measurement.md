# Conclusions — post-measurement

**Status:** CURRENT — authoritative recommendation.
**Date:** 2026-07-31
**Provenance:** final conclusions after the decisive measurement pass (findings §K).

## Measured

- trusty-search from source = **112s** (409 crates, 8 vCPU/16 GB guest, cold registry,
  workspace `lto = "thin"` confirmed applied, `ort-sys` ONNX Runtime download confirmed
  working in-guest); **162s** including a one-time 50.1s `git clone`.
- `PROVISION_SEC` from bare `tahoe-base` = **30s**.
- CoW clone **0.31s**, first boot **~34s**, subsequent boots **~18s**.
- `cargo install tga --locked` = **131s** (211 deps, cargo-default profile `lto=false`,
  4 vCPU).
- Full stack, six primary crates with shared `CARGO_TARGET_DIR` ≈ **4–8 min
  (extrapolated, NOT measured)**.

## Conclusions

1. **The 20–55 min/crate cost model is falsified** — wrong by ~20× even for the
   workspace's heaviest crate. The old figures were extrapolated from a 2-vCPU GitHub
   Actions runner.
2. **Kill the golden image.** Benefit is ~30s/run against three *observed* failure
   modes: `tart stop` write loss (§K1), a golden that shipped broken because of it, and
   a false-positive purity-gate bug. Clone `tahoe-base` and provision per run — simpler,
   reproducible from a digest-pinned base, and preserves the "cargo absent →
   guide-and-abort" negative test every run.
3. **Kill single-crate default scope.** Full-stack from-source is single-digit minutes.
4. **Size the guest, not the caching.** trusty-search (409 crates) beat tga (211 deps) —
   112s vs 131s — because of 8 vCPU vs 4. CPU allocation is the dominant lever; the
   elaborate caching strategy both research tracks proposed optimised the wrong
   variable.
5. **Never bare-`tart stop`** — it silently loses the guest's last write (reproduced 4
   of 5). Poll for observable stopped state; a tart exit code is not a completion
   signal.
6. **`tart exec` is the sole transport**; SSH demoted to opt-in interactive
   post-mortem. `tart exec` has no `--env` flag; SSH's non-interactive PATH is
   `/usr/bin:/bin:/usr/sbin:/sbin`, so `mise`, `brew` and `gh` are invisible over SSH.
   **A harness must not depend on guest shell rc files at all.**
7. **Toolchain drift is real, not theoretical** —
   `crates/trusty-git-analytics/rust-toolchain.toml` resolves to rustc **1.97.1** inside
   that directory vs the workspace-pinned **1.91.1** at root, verified in-guest. Assert
   `rustc --version` immediately before each build step.
8. **launchd works under `tart exec`** — a LaunchAgent bootstrapped via
   `launchctl bootstrap gui/$(id -u)` ran and produced its proof file with no SSH or GUI
   login. Unblocks daemon/health verification gates.
9. **Unwedge procedure** for a VM stuck `suspended`:
   `mv ~/.tart/vms/<name>/state.vzvmsave{,.bak}` then `tart run --no-graphics <name>`.
   Root cause: `tart list` derives `suspended` purely from that file's presence and
   `tart run` unconditionally tries to restore from it — the failing step — so retries
   never help.
10. **Still unmeasured:** `install.sh` end-to-end in a guest; any `--dir` mount in
    either direction; TCC under a non-iTerm2 responsible app; anything about Cirrus CLI.

## Owner scope rulings

- **Local Tart VM only** (GitHub Actions/CI out of scope).
- **Downloading all crate dependencies is acceptable** (no registry pre-warming
  required).
