---
name: project-vmtest-harness
description: vmtest-harness state — 8-phase plan closed, follow-up work continues; RC-1 open, RC-2 closed-unreachable; where the doc set lives
metadata:
  type: project
---

`vmtest-harness/` installs the whole trusty-tools stack in a disposable Tart macOS VM and asserts the install worked. Ad hoc, manually run, ~9–16 min. Three patterns differ **only in source**: (a) `released` from crates.io, (b) `branch` guest-side clone, (c) `local` working tree streamed host→guest.

**Status:** the eight-phase implementation plan is **closed** (merged through `71ff669d`, 2026-08-04). Work since then is **follow-up, not a ninth phase** — record it in `MANIFEST.md`'s open-items list, not as a new phase section.

**Standing open items:**
- **RC-1 — unified daemon health envelope. OPEN and deliberately not the plan's to close.** No shared health type in `trusty-common`; five daemons, five `/health` shapes. The oracle asserts **liveness only**. Evaluated 2026-08-04: `tctl stack health --json` recovers RC-1's *uniform classification* half but not the envelope, `version`, or a `service` discriminator — so the **#3364** squatter-collision class stays open and needs a product change.
- **RC-2 — CLOSED as unreachable-by-design** on the harness side; the product half (cargo-absent exit code/message) is still unpinned. N2 reports `BLOCKED` every run; **do not re-run the same probe.**
- Daemon time-to-ready is wholly unmeasured (`health_timeout` 60s is a judgment call).
- `install.sh` is out of scope by decision; `--dir` mounts were never measured.
- Every timing and TCC observation comes from one host, one user, iTerm2. `kTCCServiceAudioCapture` fires on VM start even with `--no-graphics` (a Virtualization.framework property), so a different launch context may prompt.

**Why:** these are the things a green run does *not* prove; the doc set exists so nobody reads a PASS line as more than it is.
**How to apply:** before proposing new assertions, check whether the thing is already recorded as a known gap with a decided reason. Do not re-litigate settled decisions (D2/D3 were reversed 2026-07-31; scope is eight crates / thirteen binaries).

**Doc set:** `docs/research/tart-vm-testing-harness/` — `02-design/01-vm-install-harness.md` (DOC-1, spec), `02-design/02-harness-contracts.md` (DOC-2, contracts), `03-plan/MANIFEST.md` (what every phase actually observed). `vmtest-harness/README.md` is self-contained for running it.

**Gotcha worth keeping:** `tctl stack doctor --json` and `tctl stack health --json` both **exit 2** when the stack is degraded. Any `set -e` shell that captures their output needs `|| :`, or it dies before reading the JSON. The harness's `_verify_doctor_json` already does this.

See [[feedback-vmtest-harness-doctrine]].
