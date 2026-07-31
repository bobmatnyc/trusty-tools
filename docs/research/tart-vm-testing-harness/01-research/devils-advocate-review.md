# Devil's advocate review

**Status:** CURRENT — adversarial review.
**Date:** 2026-07-31
**Provenance:** adversarial review of the collated recommendation against the probe's
empirical findings.

## Verdict table

| # | Claim | Verdict |
|---|-------|---------|
| 1 | Cirrus rejection | **justification KILLED, conclusion survives on unmeasured grounds** |
| 2 | bash under `scripts/vmtest/` | **SURVIVES** |
| 3 | three-stage golden image | **WEAKENED to undecidable** |
| 4 | never mount host repo | **conclusion SURVIVES, cited evidence largely FALSE** |
| 5 | single-crate scope | **premise KILLED, replacement unmeasured** |
| 6 | `install.sh` daily driver | **UNTESTED** |
| 7 | hybrid transport | **RESOLVED, "hybrid" is the wrong shape** |
| 8 | Rust 1.91 | **SURVIVES with an unflagged override trap** |
| 9 | tar transfer | **SURVIVES host-side, UNTESTED guest-side** |

## Cirrus CLI and the Local Network permission

**Local Network permission is FALSE on this host.** SSH reached full KEX and an
*authentication* failure; ICMP replied at 1.049 ms; `nc` rc=0; sshd listening
tcp4/tcp6:22; no network TCC denial in the unified log. A Local Network denial severs
traffic at packet level — none of that output is possible under one.

Track B derived the claim from *reading the cirrus-cli README*, not from running
`cirrus run`. **Zero `cirrus run` invocations exist in the probe — the Cirrus rejection
is 100% assertion.**

The strongest *real* reason nobody gave: `tart exec` is a complete transport (exact
exit codes, stdin heredocs, separated stdout/stderr, 200,000 lines untruncated, live
streaming), so the harness needs nothing from cirrus.

## Claim 4 — never mount host repo

**Claim 4's evidence does not survive source-reading.**

- `trusty-search/build.rs:45-54` returns early on `SKIP_UI_BUILD == "1"` — and both
  tracks mandate that var — so `make release-prep` (Step 3, `:63-72`) **is unreachable
  under the design's own configuration**.
- `ensure_placeholder` opens `if dist_dir.join("index.html").exists() { return; }`
  (`trusty-analyze/build.rs:123-124`) and those dirs are **tracked in git** (3 files
  under `trusty-search/ui-dist/`, 9 under `crates/*/ui/dist/`), so a clean checkout
  writes nothing.
- The calls are `let _ = std::fs::create_dir_all(...)` / `let _ = std::fs::write(...)`
  — **errors discarded** — so a read-only mount is a silent no-op, not a build failure.

The conclusion (copy, don't mount) is right for reasons nobody verified. **Zero mounts
tested in either direction.**

## The 131s interrogation

Cold registry confirmed (`bake-golden.sh:127-152` has no `cargo fetch`); 4 vCPU was the
*smallest* config. But `tga` is unrepresentative: 211 vs 395 unique normal deps, 64,836
vs 130,176 own-crate LOC; trusty-search adds `usearch` (C++ via cxx), `onig_sys`,
`esaxx-rs`, 16 tree-sitter grammars, and **`ort-sys` which downloads ONNX Runtime at
build time**.

Decisive and previously unnoticed: **`cargo install tga` from crates.io builds the
published crate as its own workspace root** with cargo defaults (`lto=false`,
`codegen-units=16`), whereas `cargo install --path` inside the workspace inherits the
root's **`lto = "thin"`**. The 131s figure does not measure the profile patterns
(b)/(c) will use.

Verdict at the time: 20–55 min is dead, but **the over-correction is equally
unsupported** — declined to substitute a number. *(Later resolved by measurement:
112s.)*

## Golden image

`~/.zshenv` — the *last* thing provisioning wrote (`bake-golden.sh:146-148`) — is
absent from the produced image while everything written earlier survived (timestamps
22:15/22:16); `verify.txt` proves it worked at bake time. The purity gate had a bug
firing in the wrong direction (commit `a37c6cbc`, fixed with `setopt NULL_GLOB`,
`:178`). **`PROVISION_SEC` — the entire economic justification — was lost when bake
stdout was lost**, though the instrument computed it correctly (`:154-156`).

Measured against it: clone 0.310s (golden, 80 GB) vs 0.306s (base, 50 GB) — identical,
an APFS `clonefile`; delete 0.29–0.30s; ~104 KB divergence per idle clone; first boot
34.4s, subsequent 18.0s.

Required fix either way: the bake verifies *before* finalize (`:160-169`) and never
after — add a post-stop clone→boot→re-verify→delete step, measured cost ~35s.

## Suspend/resume

`VZErrorDomain Code=12` reproduced twice, the second with a 5s settle. Direct blast
radius zero (cold boot 18.0s makes resume pointless) but indirectly significant:
**`tart suspend` returns rc=0 in ~0.3s while still `running`** — a tart exit code is
not a completion signal. Both tracks' `vm_destroy` is `stop` → `sleep 2` → `delete`, a
sleep-based race. The bake script gets boot right (polls until responsive, `:93-99`)
and stop right (`wait $RUN_PID`, `:225-228`); the tracks' sketches do neither.

## TCC generalises badly

`AUTHREQ_CTX: service=kTCCServiceAudioCapture, preflight=yes` fires on VM start **even
with `--no-graphics`** — a property of Virtualization.framework. It resolved
`authValue=1` with no prompt only because the responsible app already held the grant.
Logs show `responsible={identifier=com.googlecode.iterm2}, accessing={com.apple.Virtualization.VirtualMachine}`.

**Every TCC finding here — including the Local Network result — is conditional on "run
from iTerm2, this host, this user."** None transfers to a LaunchAgent, cron, a
different terminal, or SSH-into-host. Untested siblings: camera, screen recording, and
**Files-and-Folders TCC on `--dir` mounts** (a strong prompt candidate if a repo sits
under `~/Documents`/`~/Desktop`/`~/Downloads`).

## Meta

"Two independent tracks agreed" is worth much less than it looks — both extrapolated
timings from the same in-repo artifact (`e2e-docker.yml:44-47`, "~15–30 minutes on a
free GHA runner, 2 vCPU"): a shared prior, not independence.

The missed 692-line DOC-10 was **1-of-2, not 2-of-2** — Track B engages it at §2.1;
**Track A never cites it**, so Track A's entire Cirrus-vs-tart analysis re-litigated an
owner-approved decision without knowing it existed.

Roughly **half of all tested predictions were wrong or materially incomplete**; apply
that base rate to the untested half.

## Unmeasured at time of review

`PROVISION_SEC`; any from-source build of any trusty crate in a VM; the thin-LTO
asymmetry; `install.sh` end-to-end in a guest; any `--dir` mount; transfer of 81 MiB by
any mechanism; anything about Cirrus CLI; `tart stop` asynchrony; wedge recovery; TCC
under any other responsible app; the `rust-toolchain.toml` override; guest launchd; the
`ort-sys` ONNX download. *(Several were subsequently measured — see the post-measurement
conclusions.)*
